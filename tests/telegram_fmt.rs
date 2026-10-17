//! Pure-function tests for the Telegram formatting helpers.

use kongming::telegram::{markdown_to_html, split_chunks};

#[test]
fn escapes_html_special_characters() {
    assert_eq!(markdown_to_html("a < b && c > d"), "a &lt; b &amp;&amp; c &gt; d");
    assert_eq!(markdown_to_html("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
    // Inside inline code and fenced blocks too.
    assert_eq!(markdown_to_html("`a<b`"), "<code>a&lt;b</code>");
    assert_eq!(markdown_to_html("```\n<a>&\n```"), "<pre><code>&lt;a&gt;&amp;\n</code></pre>");
}

#[test]
fn bold_strike_and_underline_pairs() {
    assert_eq!(markdown_to_html("**a** and **b**"), "<b>a</b> and <b>b</b>");
    assert_eq!(markdown_to_html("__under__"), "<b>under</b>");
    assert_eq!(markdown_to_html("~~gone~~ kept"), "<s>gone</s> kept");
    // An unmatched marker is left alone.
    assert_eq!(markdown_to_html("**open"), "**open");
    assert_eq!(markdown_to_html("a ** b"), "a ** b");
}

#[test]
fn nested_formatting_and_code_protection() {
    // Markdown inside inline code is not interpreted.
    assert_eq!(markdown_to_html("`**not bold**`"), "<code>**not bold**</code>");
    // Bold around text next to code keeps both pieces intact.
    let html = markdown_to_html("**bold** then `code` then **more**");
    assert_eq!(html, "<b>bold</b> then <code>code</code> then <b>more</b>");
    // Unclosed backtick is literal.
    assert_eq!(markdown_to_html("a `b"), "a `b");
    // Links with http(s) targets become anchors; other schemes stay literal.
    assert_eq!(markdown_to_html("see [docs](https://example.com/x)"), "see <a href=\"https://example.com/x\">docs</a>");
    assert_eq!(markdown_to_html("[ftp](ftp://host/x)"), "[ftp](ftp://host/x)");
    let quoted = markdown_to_html("[q](https://e.com/?a=\"1\")");
    assert!(quoted.contains("href=\"https://e.com/?a=%221%22\""), "{quoted}");
    // Bold link text.
    assert_eq!(markdown_to_html("[**b**](https://e.com)"), "<a href=\"https://e.com\"><b>b</b></a>");
}

#[test]
fn headings_lists_and_indentation() {
    assert_eq!(markdown_to_html("# Title"), "<b>Title</b>");
    assert_eq!(markdown_to_html("### Deep **x**"), "<b>Deep <b>x</b></b>");
    assert_eq!(markdown_to_html("- item\n  - nested"), "- item\n  - nested");
    assert_eq!(markdown_to_html("1. one\n2. two"), "1. one\n2. two");
    // Trailing newlines are trimmed, blank lines inside are kept.
    assert_eq!(markdown_to_html("a\n\nb\n\n"), "a\n\nb");
    assert_eq!(markdown_to_html(""), "");
}

#[test]
fn fenced_code_blocks() {
    let md = "before\n```rust\nlet x = 1;\n# not a heading\n**not bold**\n```\nafter";
    let html = markdown_to_html(md);
    assert_eq!(html, "before\n<pre><code>let x = 1;\n# not a heading\n**not bold**\n</code></pre>\nafter");
    // An unclosed fence is closed at the end.
    assert_eq!(markdown_to_html("```\nx"), "<pre><code>x\n</code></pre>");
    // Indented fences count too.
    assert!(markdown_to_html("  ```\n  y\n  ```").starts_with("<pre><code>"));
}

#[test]
fn long_markdown_input_is_handled() {
    let mut md = String::new();
    for i in 0..500 {
        md.push_str(&format!("- item {i} with **bold {i}** and `code<{i}>` and a < b\n"));
    }
    let html = markdown_to_html(&md);
    assert_eq!(html.lines().count(), 500);
    assert!(html.contains("<b>bold 499</b>"));
    assert!(html.contains("<code>code&lt;499&gt;</code>"));
    assert!(!html.contains(" < "), "raw '<' must not survive");
    assert_eq!(html.matches("<b>").count(), 500);
    assert_eq!(html.matches("</b>").count(), 500);
}

#[test]
fn split_chunks_respects_limits_and_keeps_content() {
    let text: String = (0..300).map(|i| format!("line {i}\n")).collect();
    let chunks = split_chunks(&text, 50);
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| c.chars().count() <= 50), "a chunk exceeds the limit");
    assert!(chunks.iter().all(|c| c.ends_with('\n')), "chunks break on line boundaries");
    assert_eq!(chunks.concat(), text);

    // Short text: one chunk, unchanged.
    assert_eq!(split_chunks("hello", 100), vec!["hello".to_string()]);
    // Empty or whitespace-only input yields nothing.
    assert!(split_chunks("", 10).is_empty());
    assert!(split_chunks("  \n \n", 10).is_empty());
}

#[test]
fn split_chunks_cuts_overlong_lines_by_characters() {
    let long = "x".repeat(25);
    assert_eq!(split_chunks(&long, 10), vec!["x".repeat(10), "x".repeat(10), "x".repeat(5)]);
    // Multi-byte characters are counted as characters, not bytes.
    let accented = "é".repeat(10);
    let chunks = split_chunks(&accented, 4);
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(|c| c.chars().count() <= 4));
    assert_eq!(chunks.concat(), accented);
    // A long line in the middle of normal lines keeps everything in order.
    let mixed = format!("ab\n{}\ncd\n", "y".repeat(12));
    let chunks = split_chunks(&mixed, 5);
    assert_eq!(chunks.concat(), mixed);
    assert!(chunks.iter().all(|c| c.chars().count() <= 5));
    assert_eq!(chunks[0], "ab\n");
}

#[test]
fn split_chunks_of_rendered_html_stay_under_telegram_limit() {
    let md: String = (0..400).map(|i| format!("## Section {i}\nSome **text** for section {i}.\n\n")).collect();
    let html = markdown_to_html(&md);
    let chunks = split_chunks(&html, 4000);
    assert!(chunks.len() >= 2);
    assert!(chunks.iter().all(|c| c.chars().count() <= 4000));
    assert_eq!(chunks.concat(), html);
}
