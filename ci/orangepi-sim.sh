#!/bin/sh
# Orange Pi Zero 3 simulation for CI.
#
#   ci/orangepi-sim.sh tests <dir>        run every aarch64 test binary in <dir>
#   ci/orangepi-sim.sh smoke <kongming>   boot the aarch64 daemon against a real
#                                         libSQL server and exercise the API
#
# Both run in an arm64 container cut to the board: 4 CPUs, 1 GB RAM and no
# swap, a read-only root filesystem. On an x86 runner the container runs under
# QEMU with QEMU_CPU=cortex-a53 (the Allwinner H618 core: ARMv8.0, so no LSE
# atomics), which also makes everything several times slower, a good stress
# for timeouts and races. On an arm64 runner it runs natively and the memory
# figures are the real ones.
#
# Environment: SIM_MEMORY (default 1g) caps the daemon container,
# SIM_LABEL names the run in the job summary.
set -eu

IMAGE=busybox:1.36
BOARD="--platform linux/arm64 --cpus=4 --pids-limit=256 --read-only -e QEMU_CPU=cortex-a53"
LABEL=${SIM_LABEL:-orangepi}
SUMMARY=${GITHUB_STEP_SUMMARY:-/dev/null}

mib() { echo $(( $1 / 1048576 )); }

run_tests() {
    dir=$(cd "$1" && pwd)
    # Under QEMU every wait in the tests gets six times longer.
    timeout_secs=10
    [ "$(uname -m)" = "x86_64" ] && timeout_secs=60
    # The test harness and its mocks share the 1 GB with the code under test.
    { docker run --rm $BOARD --memory=1g --memory-swap=1g --tmpfs /tmp:size=16m \
        -e KONGMING_TEST_TIMEOUT_SECS=$timeout_secs -e RUST_BACKTRACE=1 \
        -v "$dir:/sim:ro" $IMAGE sh -c '
            uname -m; grep -m1 "CPU part" /proc/cpuinfo || true; failed=""
            for t in /sim/*; do
                echo "::group::$(basename "$t")"
                "$t" --test-threads=4 || failed="$failed $(basename "$t")"
                echo "::endgroup::"
            done
            echo "peak=$(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo 0)" > /tmp/peak
            cat /tmp/peak
            [ -z "$failed" ] || { echo "FAILED:$failed"; exit 1; }
        ' || rc=$?; echo "exit=${rc:-0}"; } 2>&1 | tee /tmp/sim-tests.log
    status=$(sed -n 's/^exit=//p' /tmp/sim-tests.log | tail -n1)
    peak=$(sed -n 's/^peak=//p' /tmp/sim-tests.log | tail -n1)
    {
        echo "### $LABEL: tests"
        echo "- test binaries: $(ls "$dir" | wc -l)"
        echo "- peak memory of the whole run (harness + mocks + app): $(mib "${peak:-0}") MiB"
    } | tee -a "$SUMMARY"
    return "${status:-1}"
}

api() { # method path [json]
    if [ $# -ge 3 ]; then
        curl -fsS -b /tmp/jar -X "$1" -H 'content-type: application/json' -d "$3" "http://127.0.0.1:18800$2"
    else
        curl -fsS -b /tmp/jar -X "$1" "http://127.0.0.1:18800$2"
    fi
}

wait_healthy() {
    i=0
    until curl -fsS http://127.0.0.1:18800/health 2>/dev/null | grep -q '"ok":true'; do
        i=$((i + 1))
        if [ $i -gt 120 ]; then
            echo "daemon never became healthy"; docker logs kongming; return 1
        fi
        sleep 1
    done
    echo "healthy after ~${i}s"
}

cleanup() {
    docker logs kongming > /tmp/kongming.log 2>&1 || true
    echo "::group::daemon log"; cat /tmp/kongming.log; echo "::endgroup::"
    docker rm -f kongming turso > /dev/null 2>&1 || true
    docker network rm orangepi > /dev/null 2>&1 || true
}

run_smoke() {
    bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
    mem=${SIM_MEMORY:-1g}
    trap cleanup EXIT
    docker network create orangepi > /dev/null
    # A real libSQL server stands in for Turso: same Hrana HTTP pipeline API.
    docker run -d --name turso --network orangepi -p 127.0.0.1:18081:8080 \
        ghcr.io/tursodatabase/libsql-server:latest > /dev/null
    i=0
    until [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18081/health)" = "200" ]; do
        i=$((i + 1))
        [ $i -le 60 ] || { echo "libSQL server never came up"; docker logs turso; return 1; }
        sleep 1
    done
    # Production conditions: read-only root, no tmpfs, no volumes but the binary.
    docker run -d --name kongming --network orangepi -p 18800:18800 \
        $BOARD --memory="$mem" --memory-swap="$mem" \
        -e KONGMING_TURSO_DATABASE_URL=http://turso:8080 -e KONGMING_TURSO_AUTH_TOKEN=unused \
        -e KONGMING_PASSWORD=orangepi -e KONGMING_SECRET=orangepi-simulation-secret \
        -v "$bin:/kongming:ro" --entrypoint /kongming $IMAGE > /dev/null
    started=$(date +%s)
    wait_healthy
    boot=$(( $(date +%s) - started ))

    curl -fsS -c /tmp/jar -H 'content-type: application/json' -d '{"password":"orangepi"}' \
        http://127.0.0.1:18800/api/auth/login > /dev/null
    api GET /api/info
    echo
    curl -fsS http://127.0.0.1:18800/ | grep -qi '<html'
    api GET /api/skills > /dev/null
    api GET /api/settings > /dev/null
    api GET /api/agents > /dev/null
    api PUT /api/agents/kongming '{"role":"Strategist"}' | grep -q '"role":"Strategist"'
    # The team is Kongming's to run: the user cannot add agents.
    [ "$(curl -sS -o /dev/null -w '%{http_code}' -b /tmp/jar -H 'content-type: application/json' \
        -d '{"handle":"scout"}' http://127.0.0.1:18800/api/agents)" = "403" ]
    api POST /api/memories '{"content":"the board is an Orange Pi Zero 3"}' > /dev/null
    sid=$(api POST /api/sessions '{"title":"sim"}' | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    [ -n "$sid" ] || { echo "no session id"; return 1; }

    # A burst: 400 requests, 8 at a time, mixed reads and writes.
    start=$(date +%s)
    seq 1 400 | xargs -P 8 -I{} sh -c '
        case $(( {} % 4 )) in
            0) curl -fsS -o /dev/null http://127.0.0.1:18800/health ;;
            1) curl -fsS -o /dev/null -b /tmp/jar http://127.0.0.1:18800/api/sessions ;;
            2) curl -fsS -o /dev/null -b /tmp/jar http://127.0.0.1:18800/api/agents ;;
            3) curl -fsS -o /dev/null -b /tmp/jar -X PATCH -H "content-type: application/json" \
                   -d "{\"title\":\"sim {}\"}" http://127.0.0.1:18800/api/sessions/'"$sid"' ;;
        esac'
    burst=$(( $(date +%s) - start ))
    peak=$(docker exec kongming cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo 0)
    current=$(docker exec kongming cat /sys/fs/cgroup/memory.current 2>/dev/null || echo 0)

    # Nothing lives on the box: after a restart everything comes back from the database.
    docker restart kongming > /dev/null
    wait_healthy
    curl -fsS -c /tmp/jar -H 'content-type: application/json' -d '{"password":"orangepi"}' \
        http://127.0.0.1:18800/api/auth/login > /dev/null
    api GET /api/agents | grep -q '"role":"Strategist"'
    api GET "/api/sessions/$sid" | grep -q '"title":"sim '

    [ "$(docker inspect -f '{{.State.OOMKilled}}' kongming)" = "false" ]
    if docker logs kongming 2>&1 | grep -q 'panicked'; then
        docker logs kongming; echo "the daemon panicked"; return 1
    fi
    {
        echo "### $LABEL: daemon smoke test"
        echo "- memory limit: $mem, boot to healthy: ~${boot}s"
        echo "- 400 mixed requests (8 concurrent): ${burst}s"
        echo "- container memory after the burst: $(mib "$current") MiB, peak: $(mib "$peak") MiB"
        [ "$(uname -m)" = "x86_64" ] && echo "- under QEMU these figures include the emulator; the arm64 run has the real ones"
        echo "- restart: agents and sessions came back from the database"
    } | tee -a "$SUMMARY"
}

case "${1:-}" in
    tests) run_tests "$2" ;;
    smoke) run_smoke "$2" ;;
    *) echo "usage: $0 tests <dir> | smoke <kongming binary>" >&2; exit 2 ;;
esac
