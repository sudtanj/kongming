/* kongming web UI — dependency-free vanilla JS, no build step.
   Modules, in order: i18n · util · api · theme · markdown · store · ws · router ·
   views (shell, sidebar, thread, composer, palettes, settings pages, modals) · boot.
   Routes: #/  ·  #/s/<sessionId>  ·  #/settings/<page>[?query] */
(() => {
'use strict';

/* ============================================================ i18n */

const STRINGS = {
  en: {
    'app.name': 'kongming',
    'app.loading': 'Loading kongming…',
    'nav.newChat': 'New chat',
    'nav.settings': 'Settings',
    'nav.agents': 'Agents',
    'nav.skills': 'Skills',
    'nav.automations': 'Automations',
    'nav.channels': 'Channels',
    'nav.toggleSidebar': 'Toggle sidebar',
    'nav.closeSidebar': 'Close sidebar',
    'group.today': 'Today',
    'group.yesterday': 'Yesterday',
    'group.earlier': 'Earlier',
    'group.running': 'Running',
    'session.untitled': 'New chat',
    'session.rename': 'Rename',
    'session.delete': 'Delete',
    'session.empty': 'No conversations yet. Start one below.',
    'session.deleteTitle': 'Delete this chat?',
    'session.deleteDesc': 'The conversation and its history are removed permanently.',
    'session.clearTitle': 'Clear this chat?',
    'session.clearDesc': 'Messages are deleted but the chat stays.',
    'session.running': 'Replying…',
    'session.info': '{{channel}} · {{count}} messages',
    'common.cancel': 'Cancel',
    'common.delete': 'Delete',
    'common.save': 'Save',
    'common.saved': 'Saved',
    'common.saving': 'Saving…',
    'common.create': 'Create',
    'common.edit': 'Edit',
    'common.close': 'Close',
    'common.add': 'Add',
    'common.copy': 'Copy',
    'common.copied': 'Copied',
    'common.retry': 'Retry',
    'common.loading': 'Loading…',
    'common.none': 'None',
    'common.enabled': 'Enabled',
    'common.disabled': 'Disabled',
    'common.yes': 'Yes',
    'common.no': 'No',
    'common.never': 'Never',
    'common.error': 'Something went wrong',
    'common.confirm': 'Confirm',
    'common.back': 'Back',
    'common.optional': 'optional',
    'common.all': 'All',
    'common.default': 'Default',
    'common.unknown': 'unknown',
    'conn.online': 'Connected',
    'conn.offline': 'Offline',
    'conn.reconnecting': 'Reconnecting…',
    'conn.lagged': 'Connection lagged; refreshing…',
    'greeting.workOn': 'What should we work on?',
    'greeting.start': 'Where should we start?',
    'greeting.build': 'What are we building today?',
    'greeting.tackle': 'What should we tackle together?',
    'hero.sub': 'Talk to {{agent}} — mention teammates with @, run commands with /.',
    'hero.quick.plan': 'Make a plan',
    'hero.quick.planPrompt': 'Create a concise plan for what I should do next.',
    'hero.quick.skills': 'What can you do?',
    'hero.quick.skillsPrompt': 'List your skills and the tools you have available.',
    'hero.quick.team': 'Who is on the team?',
    'hero.quick.teamPrompt': 'Introduce the agents on the team and what each is responsible for.',
    'composer.placeholder': 'Message {{agent}}…',
    'composer.placeholderGeneric': 'Send a message…',
    'composer.send': 'Send',
    'composer.stop': 'Stop',
    'composer.mention': 'Mention an agent, skill, session or MCP server',
    'composer.commands': 'Slash commands',
    'composer.attach': 'Attachments are not available in kongming',
    'composer.hint': 'Enter to send · Shift+Enter for a new line',
    'composer.agent': 'Agent',
    'composer.pickAgent': 'Choose an agent',
    'composer.chars': '{{count}} characters',
    'palette.agents': 'Agents',
    'palette.skills': 'Skills',
    'palette.sessions': 'Sessions',
    'palette.mcp': 'MCP servers',
    'palette.commands': 'Commands',
    'palette.empty': 'No matches',
    'cmd.new': 'Start a new chat',
    'cmd.clear': 'Clear this chat',
    'cmd.agent': 'Switch this chat to another agent',
    'cmd.stop': 'Stop the current reply',
    'cmd.help': 'Show available commands',
    'cmd.helpText': 'Commands: /new, /clear, /agent <handle>, /stop, /help. Mention teammates with @handle, attach skills with @skill:name, sessions with @session:id, MCP servers with @mcp:name.',
    'cmd.agentUsage': 'Usage: /agent <handle>',
    'cmd.noAgent': 'No agent named @{{handle}}',
    'cmd.switched': 'Switched to @{{handle}}',
    'cmd.nothingToStop': 'Nothing is running',
    'cmd.needSession': 'Open a chat first',
    'thread.thinking': 'Thinking',
    'thread.thinkingLive': 'Thinking…',
    'thread.toolOne': 'Used {{name}}',
    'thread.toolMany': 'Used {{count}} tools',
    'thread.toolLive': 'Using {{name}}…',
    'thread.toolsLive': 'Working ({{count}} tools)…',
    'thread.working': 'Working…',
    'thread.args': 'Arguments',
    'thread.result': 'Result',
    'thread.pending': 'running',
    'thread.error': 'error',
    'thread.done': 'done',
    'thread.stopped': 'Stopped',
    'thread.failed': 'The reply failed',
    'thread.sending': 'Sending…',
    'thread.notSent': 'Not sent',
    'thread.loadFailed': 'Could not load this conversation',
    'thread.cleared': 'Chat cleared',
    'thread.copyMessage': 'Copy message',
    'thread.delegated': 'Delegated task',
    'login.title': 'Sign in to kongming',
    'login.desc': 'This instance is password protected.',
    'login.password': 'Password',
    'login.submit': 'Sign in',
    'login.wrong': 'Wrong password',
    'login.logout': 'Sign out',
    'settings.title': 'Settings',
    'settings.overview': 'Overview',
    'settings.appearance': 'Appearance',
    'settings.models': 'Models',
    'settings.agents': 'Agents',
    'settings.skills': 'Skills',
    'settings.mcp': 'MCP',
    'settings.channels': 'Channels',
    'settings.automations': 'Automations',
    'settings.memory': 'Memory',
    'settings.activity': 'Activity',
    'settings.about': 'About',
    'settings.backToChat': 'Back to chat',
    'theme.title': 'Theme',
    'theme.desc': 'Follow the system or pick a side.',
    'theme.system': 'System',
    'theme.light': 'Light',
    'theme.dark': 'Dark',
    'appearance.sidebar': 'Sidebar',
    'appearance.sidebarDesc': 'Show the conversation list on wide screens.',
    'appearance.motion': 'Reduced motion',
    'appearance.motionDesc': 'Follows your operating system preference.',
    'overview.status': 'Status',
    'overview.version': 'Version',
    'overview.uptime': 'Uptime',
    'overview.db': 'Database',
    'overview.llm': 'Model',
    'overview.telegram': 'Telegram',
    'overview.agents': 'Agents',
    'overview.usage': 'Usage (24h)',
    'overview.requests': 'requests',
    'overview.promptTokens': 'prompt tokens',
    'overview.completionTokens': 'completion tokens',
    'overview.publicUrl': 'Public URL',
    'overview.notSet': 'not set',
    'overview.auth': 'Password protected',
    'models.provider': 'Provider',
    'models.providerDesc': 'OpenAI-compatible or Anthropic Messages API.',
    'models.baseUrl': 'Base URL',
    'models.baseUrlDesc': 'Leave empty for the provider default.',
    'models.apiKey': 'API key',
    'models.apiKeyDesc': 'Stored in the database. Leave blank to keep the current key.',
    'models.apiKeySet': 'A key is set',
    'models.apiKeyNone': 'No key',
    'models.clearKey': 'Clear key',
    'models.model': 'Model',
    'models.maxTokens': 'Max tokens',
    'models.temperature': 'Temperature',
    'models.effort': 'Reasoning effort',
    'models.test': 'Test connection',
    'models.testing': 'Testing…',
    'models.testOk': 'Replied "{{reply}}" in {{ms}} ms ({{model}})',
    'models.reflection': 'Self-improvement',
    'models.reflectionDesc': 'Reflect after turns and save lessons as skills and memories.',
    'models.publicUrl': 'Public URL',
    'models.publicUrlDesc': 'Optional. When kongming is reachable at this address, MCP OAuth returns here automatically; leave empty behind a firewall to paste the redirect URL instead.',
    'agents.title': 'Agents',
    'agents.desc': 'Your team. Managers can delegate to the agents that report to them.',
    'agents.new': 'New agent',
    'agents.orgChart': 'Org chart',
    'agents.handle': 'Handle',
    'agents.handleDesc': 'Used in @mentions. Lowercase letters, digits, - and _.',
    'agents.name': 'Name',
    'agents.role': 'Role',
    'agents.roleDesc': 'Job title, e.g. Researcher.',
    'agents.description': 'Responsibilities',
    'agents.descriptionDesc': 'Shown to other agents so they can route work.',
    'agents.systemPrompt': 'System prompt',
    'agents.model': 'Model override',
    'agents.modelDesc': 'Leave empty to use the global model.',
    'agents.manager': 'Reports to',
    'agents.tools': 'Built-in tools',
    'agents.toolsDesc': 'Select none to allow all.',
    'agents.mcp': 'MCP servers',
    'agents.mcpDesc': 'Select none to allow all enabled servers.',
    'agents.pinned': 'Pinned skills',
    'agents.pinnedDesc': 'Always loaded into the prompt.',
    'agents.selfImprove': 'Self-improve',
    'agents.selfImproveDesc': 'May create and update skills and memories on its own.',
    'agents.isDefault': 'Default agent',
    'agents.isDefaultDesc': 'Used for new chats and unaddressed messages.',
    'agents.deleteTitle': 'Delete @{{handle}}?',
    'agents.deleteDesc': 'Sessions keep their history, but nobody will answer in them until you pick another agent.',
    'agents.reports': 'reports to @{{handle}}',
    'agents.noReports': 'top level',
    'agents.chat': 'Chat',
    'skills.title': 'Skills',
    'skills.desc': 'Reusable procedures in SKILL.md format. Agents load them on demand or when pinned.',
    'skills.import': 'Import SKILL.md',
    'skills.importDesc': 'Paste a SKILL.md document with name and description frontmatter.',
    'skills.new': 'New skill',
    'skills.uses': '{{count}} uses',
    'skills.version': 'v{{version}}',
    'skills.source': 'by {{source}}',
    'skills.name': 'Name',
    'skills.description': 'Description',
    'skills.tags': 'Tags',
    'skills.tagsDesc': 'Comma separated.',
    'skills.body': 'Body (Markdown)',
    'skills.preview': 'Preview',
    'skills.edit': 'Edit',
    'skills.history': 'Version history',
    'skills.export': 'Copy SKILL.md',
    'skills.deleteTitle': 'Delete skill "{{name}}"?',
    'skills.deleteDesc': 'All versions are removed.',
    'skills.empty': 'No skills yet. Import one or let an agent learn.',
    'skills.restore': 'Load this version into the editor',
    'mcp.title': 'MCP servers',
    'mcp.desc': 'Remote Model Context Protocol servers over HTTP. Their tools are offered to agents.',
    'mcp.add': 'Add server',
    'mcp.name': 'Name',
    'mcp.url': 'URL',
    'mcp.transport': 'Transport',
    'mcp.authType': 'Authentication',
    'mcp.token': 'Bearer token',
    'mcp.tokenDesc': 'Leave blank to keep the stored token.',
    'mcp.clearToken': 'Clear token',
    'mcp.oauthClientId': 'OAuth client ID',
    'mcp.oauthClientSecret': 'OAuth client secret',
    'mcp.oauthScope': 'OAuth scope',
    'mcp.oauthDesc': 'Only for servers without dynamic client registration.',
    'mcp.headers': 'Extra headers',
    'mcp.refresh': 'Refresh',
    'mcp.connect': 'Connect with OAuth',
    'mcp.disconnect': 'Disconnect OAuth',
    'mcp.tools': '{{count}} tools',
    'mcp.showTools': 'Tools',
    'mcp.tryTool': 'Try a tool',
    'mcp.toolName': 'Tool',
    'mcp.args': 'Arguments (JSON)',
    'mcp.call': 'Call',
    'mcp.deleteTitle': 'Remove "{{name}}"?',
    'mcp.deleteDesc': 'Agents lose access to its tools.',
    'mcp.empty': 'No MCP servers configured.',
    'mcp.oauthOk': 'OAuth connected',
    'mcp.oauthErr': 'OAuth failed: {{message}}',
    'mcp.oauthTitle': 'Connect {{name}}',
    'mcp.oauthManualDesc': 'kongming is not reachable from the internet, so the sign-in page cannot send you back automatically. Sign in in a new tab, then paste the address you end up on.',
    'mcp.oauthStep1': '1. Sign in with the provider',
    'mcp.oauthOpen': 'Open sign-in page',
    'mcp.oauthCopyLink': 'Copy link',
    'mcp.oauthStep2': '2. Paste the redirect URL',
    'mcp.oauthStep2Desc': 'After approving, the browser goes to {{uri}}. That page usually fails to load; that is expected. Copy the full address from the address bar and paste it here.',
    'mcp.oauthComplete': 'Complete sign-in',
    'mcp.status.connected': 'connected',
    'mcp.status.error': 'error',
    'mcp.status.unauthorized': 'unauthorized',
    'mcp.status.disabled': 'disabled',
    'mcp.status.unknown': 'unknown',
    'channels.title': 'Channels',
    'channels.desc': 'Where kongming listens besides this web UI.',
    'channels.telegram': 'Telegram',
    'channels.token': 'Bot token',
    'channels.tokenDesc': 'From @BotFather. Saving checks it with Telegram and connects the bot. Leave blank to keep the stored token.',
    'channels.connection': 'Connection',
    'channels.connectionDesc': 'Saving a bot token checks it with Telegram and sets up delivery automatically.',
    'channels.connectedAs': 'Connected as {{bot}}',
    'channels.notConnected': 'Not connected',
    'channels.connecting': 'Connecting…',
    'channels.notSetUp': 'Not set up',
    'channels.modeWebhook': 'Webhook: Telegram pushes updates to {{url}}',
    'channels.modePolling': 'Long polling: kongming fetches updates from Telegram, so nothing has to reach this machine. Set a public HTTPS URL in Settings to use a webhook instead.',
    'channels.tokenSet': 'Token set',
    'channels.tokenNone': 'No token',
    'channels.clearToken': 'Clear token',
    'channels.allowed': 'Allowed user IDs',
    'channels.allowedDesc': 'Comma separated numeric Telegram user IDs. Empty allows nobody.',
    'channels.enabled': 'Enable Telegram',
    'automations.title': 'Automations',
    'automations.desc': 'Scheduled prompts. Schedules run in UTC.',
    'automations.new': 'New automation',
    'automations.name': 'Name',
    'automations.agent': 'Agent',
    'automations.schedule': 'Schedule',
    'automations.scheduleDesc': 'every:30 (minutes) or a 5-field cron expression, UTC.',
    'automations.prompt': 'Prompt',
    'automations.deliverTo': 'Deliver to Telegram chat',
    'automations.deliverToDesc': 'Chat ID, optional.',
    'automations.runNow': 'Run now',
    'automations.started': 'Run started',
    'automations.last': 'last {{when}} · {{status}}',
    'automations.next': 'next {{when}}',
    'automations.deleteTitle': 'Delete automation "{{name}}"?',
    'automations.empty': 'No automations yet.',
    'memory.title': 'Memory',
    'memory.desc': 'Facts, preferences and lessons agents recall when relevant.',
    'memory.add': 'Add memory',
    'memory.kind': 'Kind',
    'memory.content': 'Content',
    'memory.keywords': 'Keywords',
    'memory.importance': 'Importance (1–5)',
    'memory.scope': 'Scope',
    'memory.shared': 'Shared',
    'memory.empty': 'Nothing remembered yet.',
    'activity.title': 'Activity',
    'activity.desc': 'What the team learned and delegated.',
    'activity.improvements': 'Self-improvement log',
    'activity.tasks': 'Delegations',
    'activity.usage': 'Token usage',
    'activity.day': 'Today',
    'activity.week': '7 days',
    'activity.month': '30 days',
    'activity.openSession': 'Open session',
    'activity.empty': 'Nothing here yet.',
    'activity.kind.skill_created': 'Skill created',
    'activity.kind.skill_updated': 'Skill updated',
    'activity.kind.memory_saved': 'Memory saved',
    'activity.kind.reflection': 'Reflection',
    'toast.learned': 'Learned: {{note}}',
    'about.title': 'About',
    'about.desc': 'kongming is a small agentic daemon: a team of agents, skills that grow, and tools over MCP.',
    'about.ui': 'Interface modeled after the nanobot web UI, rebuilt without dependencies.',
    'time.justNow': 'just now',
    'time.minutes': '{{n}}m ago',
    'time.hours': '{{n}}h ago',
    'time.days': '{{n}}d ago',
  },
};
let locale = 'en';
function t(key, vars) {
  let s = (STRINGS[locale] && STRINGS[locale][key]) || STRINGS.en[key] || key;
  if (vars) s = s.replace(/\{\{(\w+)\}\}/g, (_, k) => (vars[k] == null ? '' : String(vars[k])));
  return s;
}

/* ============================================================ util */

const $ = (sel, root) => (root || document).querySelector(sel);
const SVG_NS = 'http://www.w3.org/2000/svg';

/** h(tag, attrs?, ...children): tiny element builder. attrs: class, on<Event>, dataset, aria-*, boolean props. */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const k of Object.keys(attrs)) {
      const v = attrs[k];
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k === 'html') el.innerHTML = v; // only ever fed by the markdown renderer
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
      else if (k === 'dataset') Object.assign(el.dataset, v);
      else if (k === 'style') el.style.cssText = v;
      else if (k in el && typeof v !== 'string' && k !== 'value') el[k] = v;
      else el.setAttribute(k, v === true ? '' : v);
    }
  }
  append(el, children);
  return el;
}
function append(el, children) {
  for (const c of children) {
    if (c == null || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else el.appendChild(typeof c === 'string' || typeof c === 'number' ? document.createTextNode(String(c)) : c);
  }
  return el;
}
function icon(name, cls) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'i' + (cls ? ' ' + cls : ''));
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', '#i-' + name);
  svg.appendChild(use);
  return svg;
}
function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); return el; }
function esc(s) { return String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]); }
function hue(str) { let x = 0; for (const ch of String(str || '')) x = (x * 31 + ch.charCodeAt(0)) >>> 0; return x % 360; }
function uid() { return Math.random().toString(36).slice(2, 10) + Date.now().toString(36); }
function fmtTime(ms) { if (!ms) return ''; const d = new Date(ms); return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); }
function fmtDate(ms) { if (!ms) return '—'; const d = new Date(ms); return d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }); }
function relTime(ms) {
  if (!ms) return t('common.never');
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return t('time.justNow');
  if (s < 3600) return t('time.minutes', { n: Math.floor(s / 60) });
  if (s < 86400) return t('time.hours', { n: Math.floor(s / 3600) });
  return t('time.days', { n: Math.floor(s / 86400) });
}
function fmtDuration(ms) { if (ms == null) return '—'; const s = Math.floor(ms / 1000); const d = Math.floor(s / 86400), hh = Math.floor((s % 86400) / 3600), mm = Math.floor((s % 3600) / 60); return d ? `${d}d ${hh}h` : hh ? `${hh}h ${mm}m` : `${mm}m ${s % 60}s`; }
function fmtNum(n) { return Number(n || 0).toLocaleString(); }
function dayKey(ms) { const d = new Date(ms); return d.getFullYear() * 10000 + d.getMonth() * 100 + d.getDate(); }
function copyText(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) return navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
  return Promise.resolve(fallbackCopy(text));
}
function fallbackCopy(text) {
  const ta = h('textarea', { style: 'position:fixed;opacity:0', value: text });
  document.body.appendChild(ta); ta.select();
  try { document.execCommand('copy'); } catch (e) { /* ignore */ }
  ta.remove();
}
function safeJson(v) { try { return typeof v === 'string' ? v : JSON.stringify(v, null, 2); } catch (e) { return String(v); } }
function argSummary(args) {
  if (args == null) return '';
  if (typeof args === 'string') return args;
  const parts = [];
  for (const k of Object.keys(args)) { const v = args[k]; parts.push(`${k}: ${typeof v === 'string' ? v : JSON.stringify(v)}`); }
  return parts.join(', ').slice(0, 160);
}
function debounce(fn, ms) { let tm; return (...a) => { clearTimeout(tm); tm = setTimeout(() => fn(...a), ms); }; }
const reducedMotion = () => window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/* ============================================================ api */

class ApiError extends Error { constructor(status, message) { super(message); this.status = status; } }
async function req(method, path, body) {
  const init = { method, credentials: 'same-origin', headers: {} };
  if (body !== undefined) { init.headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(body); }
  let res;
  try { res = await fetch(path, init); } catch (e) { throw new ApiError(0, t('conn.offline')); }
  let data = null;
  const text = await res.text();
  if (text) { try { data = JSON.parse(text); } catch (e) { data = { error: text.slice(0, 200) }; } }
  if (res.status === 401 && S.auth.required) { S.auth.authenticated = false; renderRoot(); }
  if (!res.ok) throw new ApiError(res.status, (data && data.error) || res.statusText || t('common.error'));
  return data;
}
const api = {
  get: (p) => req('GET', p),
  post: (p, b) => req('POST', p, b === undefined ? {} : b),
  put: (p, b) => req('PUT', p, b),
  patch: (p, b) => req('PATCH', p, b),
  del: (p) => req('DELETE', p),
};

/* ============================================================ theme */

const THEME_KEY = 'kongming.theme';
const theme = {
  get() { try { return localStorage.getItem(THEME_KEY) || 'system'; } catch (e) { return 'system'; } },
  set(mode) { try { if (mode === 'system') localStorage.removeItem(THEME_KEY); else localStorage.setItem(THEME_KEY, mode); } catch (e) { /* ignore */ } theme.apply(); },
  isDark() { const m = theme.get(); return m === 'dark' || (m === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches); },
  apply() {
    const dark = theme.isDark();
    document.documentElement.classList.toggle('dark', dark);
    const meta = $('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', meta.getAttribute(dark ? 'data-theme-color-dark' : 'data-theme-color-light'));
  },
};
if (window.matchMedia) window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => theme.apply());

/* ============================================================ markdown */

const md = (() => {
  const SAFE_URL = /^(https?:|mailto:|tel:|\/|#|\.\/)/i;
  const SAFE_IMG = /^(https?:|data:image\/(png|jpe?g|gif|webp|svg\+xml);|\/)/i;
  function safeUrl(u) { const s = u.replace(/&amp;/g, '&').trim(); return SAFE_URL.test(s) ? esc(s) : '#'; }
  function inline(raw) {
    let s = esc(raw);
    const hold = [];
    const keep = (html) => { hold.push(html); return `\u0000${hold.length - 1}\u0000`; };
    s = s.replace(/`([^`\n]+)`/g, (_, c) => keep(`<code>${c}</code>`));
    s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)/g, (_, alt, src) => SAFE_IMG.test(src) ? keep(`<img src="${esc(src)}" alt="${alt}" loading="lazy">`) : alt);
    s = s.replace(/\[([^\]]+)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)/g, (_, txt, url) => keep(`<a href="${safeUrl(url)}" target="_blank" rel="noopener noreferrer">${txt}</a>`));
    s = s.replace(/(^|[\s(])(https?:\/\/[^\s<]+?)(?=[.,;:!?)]*(?:\s|$))/g, (_, pre, url) => pre + keep(`<a href="${esc(url)}" target="_blank" rel="noopener noreferrer">${url}</a>`));
    s = s.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>').replace(/__(.+?)__/g, '<strong>$1</strong>');
    s = s.replace(/(^|[^\w*])\*([^*\n]+?)\*(?![\w*])/g, '$1<em>$2</em>').replace(/(^|[^\w_])_([^_\n]+?)_(?![\w_])/g, '$1<em>$2</em>');
    s = s.replace(/~~(.+?)~~/g, '<del>$1</del>');
    s = s.replace(/\n/g, '<br>');
    return s.replace(/\u0000(\d+)\u0000/g, (_, i) => hold[+i]);
  }
  function isHr(l) { return /^ {0,3}([-*_])( *\1){2,} *$/.test(l); }
  function tableRow(l) { return l.trim().replace(/^\||\|$/g, '').split('|').map((c) => c.trim()); }
  function isTableSep(l) { return /^ *\|? *:?-{2,}:? *(\| *:?-{2,}:? *)*\|? *$/.test(l) && l.includes('-'); }
  function render(src) {
    const lines = String(src || '').replace(/\r\n?/g, '\n').split('\n');
    const out = [];
    let i = 0;
    const para = [];
    const flushPara = () => { if (para.length) { out.push(`<p>${inline(para.join('\n'))}</p>`); para.length = 0; } };
    while (i < lines.length) {
      const line = lines[i];
      let m;
      if ((m = /^ {0,3}(`{3,}|~{3,})\s*([\w+#.-]*)/.exec(line))) {
        flushPara();
        const fence = m[1][0]; const lang = m[2]; const buf = [];
        i++;
        while (i < lines.length && !new RegExp(`^ {0,3}${fence}{3,}\\s*$`).test(lines[i])) buf.push(lines[i++]);
        i++;
        out.push(`<pre><div class="code-head"><span>${esc(lang || 'text')}</span><button type="button" data-copy aria-label="${t('common.copy')}">${t('common.copy')}</button></div><code class="lang-${esc(lang)}">${esc(buf.join('\n'))}</code></pre>`);
        continue;
      }
      if (!line.trim()) { flushPara(); i++; continue; }
      if ((m = /^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line))) { flushPara(); out.push(`<h${m[1].length}>${inline(m[2])}</h${m[1].length}>`); i++; continue; }
      if (isHr(line)) { flushPara(); out.push('<hr>'); i++; continue; }
      if (/^ {0,3}>/.test(line)) {
        flushPara(); const buf = [];
        while (i < lines.length && /^ {0,3}>/.test(lines[i])) buf.push(lines[i++].replace(/^ {0,3}> ?/, ''));
        out.push(`<blockquote>${render(buf.join('\n'))}</blockquote>`); continue;
      }
      if (/^\s*\|?.*\|.*$/.test(line) && i + 1 < lines.length && isTableSep(lines[i + 1])) {
        flushPara();
        const head = tableRow(line); const aligns = tableRow(lines[i + 1]).map((c) => /^:-+:$/.test(c) ? 'center' : /-+:$/.test(c) ? 'right' : '');
        i += 2; const rows = [];
        while (i < lines.length && lines[i].includes('|') && lines[i].trim()) rows.push(tableRow(lines[i++]));
        const cell = (tag, c, j) => `<${tag}${aligns[j] ? ` style="text-align:${aligns[j]}"` : ''}>${inline(c)}</${tag}>`;
        out.push(`<table><thead><tr>${head.map((c, j) => cell('th', c, j)).join('')}</tr></thead><tbody>${rows.map((r) => `<tr>${head.map((_, j) => cell('td', r[j] || '', j)).join('')}</tr>`).join('')}</tbody></table>`);
        continue;
      }
      if ((m = /^(\s*)([-*+]|\d+[.)])\s+/.exec(line))) {
        flushPara();
        const ordered = /\d/.test(m[2]); const baseIndent = m[1].length; const items = [];
        while (i < lines.length) {
          const l = lines[i]; const mm = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(l);
          if (mm && mm[1].length <= baseIndent) { if (mm[1].length < baseIndent) break; items.push([mm[3]]); i++; continue; }
          if (!l.trim()) { if (i + 1 < lines.length && /^\s+\S/.test(lines[i + 1]) && items.length) { items[items.length - 1].push(''); i++; continue; } break; }
          if (/^\s+/.test(l) && items.length) { items[items.length - 1].push(l.replace(new RegExp(`^ {0,${baseIndent + 2}}`), '')); i++; continue; }
          break;
        }
        const start = ordered ? parseInt(m[2], 10) : 1;
        const li = items.map((it) => {
          let first = it[0]; let cls = '';
          const task = /^\[([ xX])\]\s+/.exec(first);
          if (task) { first = `<input type="checkbox" disabled${task[1] !== ' ' ? ' checked' : ''}>` + first.slice(task[0].length); cls = ' class="task"'; }
          const rest = it.slice(1).join('\n');
          const nested = rest.trim() ? render(rest) : '';
          return `<li${cls}>${task ? first.replace(/^(<input[^>]*>)([\s\S]*)$/, (_, a, b) => a + inline(b)) : inline(first)}${nested}</li>`;
        }).join('');
        out.push(ordered ? `<ol${start !== 1 ? ` start="${start}"` : ''}>${li}</ol>` : `<ul>${li}</ul>`);
        continue;
      }
      para.push(line); i++;
    }
    flushPara();
    return out.join('\n');
  }
  return { render, inline };
})();

/* ============================================================ store */

const S = {
  auth: { required: false, authenticated: true },
  route: { name: 'chat', id: null, page: 'overview', query: {} },
  agents: [], sessions: [], skills: [], mcp: [],
  current: null,            // { id, session, messages, agent, running, pendingUser }
  live: new Map(),          // sessionId -> { turnId, agent, text, reasoning, tools: [], status, error, stopReason }
  ws: 'offline',
  pendingAgentId: null,     // agent chosen in the hero before the session exists
  sidebarCollapsed: false, sidebarOpen: false,
  info: null,
  greeting: ['workOn', 'start', 'build', 'tackle'][Math.floor(Math.random() * 4)],
};
try { S.sidebarCollapsed = localStorage.getItem('kongming.sidebar') === 'collapsed'; } catch (e) { /* ignore */ }

const agentById = (id) => S.agents.find((a) => a.id === id);
const agentByHandle = (hd) => S.agents.find((a) => a.handle === String(hd || '').toLowerCase());
const defaultAgent = () => S.agents.find((a) => a.isDefault) || S.agents[0];
function sessionAgent() { return (S.current && S.current.agent) || (S.pendingAgentId && agentById(S.pendingAgentId)) || defaultAgent(); }
function liveFor(id, create) { let l = S.live.get(id); if (!l && create) { l = { turnId: null, agent: null, text: '', reasoning: '', tools: [], status: '', error: null, stopReason: null }; S.live.set(id, l); } return l; }

async function loadAgents() { S.agents = await api.get('/api/agents'); }
async function loadSessions() { S.sessions = await api.get('/api/sessions?limit=200'); renderSidebarList(); }
async function loadSkills() { S.skills = await api.get('/api/skills'); return S.skills; }
async function loadMcp() { S.mcp = await api.get('/api/mcp'); return S.mcp; }

function upsertSession(sess, extra) {
  const i = S.sessions.findIndex((s) => s.id === sess.id);
  const prev = i >= 0 ? S.sessions[i] : {};
  const merged = Object.assign({}, prev, sess, extra || {});
  if (!merged.agentHandle || (extra && extra.agentHandle === undefined && sess.agentId !== prev.agentId)) { const a = agentById(merged.agentId); merged.agentHandle = a ? a.handle : merged.agentHandle; }
  if (i >= 0) S.sessions[i] = merged; else S.sessions.unshift(merged);
  S.sessions.sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0));
  if (S.current && S.current.id === merged.id) S.current.session = Object.assign({}, S.current.session, merged);
  renderSidebarList();
}

/* ============================================================ ws */

const ws = (() => {
  let sock = null, backoff = 1000, timer = null, wasOpen = false, closedByUs = false;
  const pending = new Map();
  function url() { return (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws'; }
  function connect() {
    if (sock && (sock.readyState === WebSocket.OPEN || sock.readyState === WebSocket.CONNECTING)) return;
    closedByUs = false;
    try { sock = new WebSocket(url()); } catch (e) { schedule(); return; }
    sock.onopen = () => {
      backoff = 1000; setStatus('online');
      if (wasOpen) resyncAfterReconnect();
      wasOpen = true;
    };
    sock.onmessage = (ev) => { let f; try { f = JSON.parse(ev.data); } catch (e) { return; } onFrame(f); };
    sock.onclose = () => { sock = null; if (!closedByUs) { setStatus('reconnecting'); schedule(); } };
    sock.onerror = () => { /* onclose follows */ };
  }
  function schedule() { clearTimeout(timer); timer = setTimeout(connect, backoff); backoff = Math.min(backoff * 1.8, 30000); }
  function setStatus(s) { S.ws = s; const b = $('#conn-badge'); if (b) { b.className = 'conn-badge ' + s; b.textContent = t('conn.' + s); b.title = t('conn.' + s); } }
  function send(frame) {
    return new Promise((resolve, reject) => {
      if (!sock || sock.readyState !== WebSocket.OPEN) { reject(new Error(t('conn.offline'))); return; }
      const requestId = uid();
      pending.set(requestId, { resolve, reject, at: Date.now() });
      sock.send(JSON.stringify(Object.assign({ requestId }, frame)));
      setTimeout(() => { const p = pending.get(requestId); if (p) { pending.delete(requestId); p.reject(new Error('timeout')); } }, 15000);
    });
  }
  function onFrame(f) {
    if (f.requestId && pending.has(f.requestId)) {
      const p = pending.get(f.requestId); pending.delete(f.requestId);
      if (f.type === 'error') p.reject(new ApiError(0, f.message || 'error')); else p.resolve(f);
      return;
    }
    switch (f.type) {
      case 'ready': case 'pong': case 'accepted': case 'stopped': return;
      case 'lagged': toast(t('conn.lagged')); resyncAfterReconnect(); return;
      case 'error': if (f.message) toast(f.message, 'err'); return;
      default: handleEvent(f);
    }
  }
  async function resyncAfterReconnect() {
    try { await loadSessions(); } catch (e) { /* ignore */ }
    if (S.current) { try { await openSession(S.current.id, { force: true }); } catch (e) { /* ignore */ } }
  }
  function close() { closedByUs = true; clearTimeout(timer); if (sock) sock.close(); sock = null; setStatus('offline'); }
  return { connect, send, close, setStatus };
})();

/** Server Event frames (src/state.rs). All sessions arrive here; filter by sessionId. */
function handleEvent(ev) {
  const sid = ev.sessionId || (ev.session && ev.session.id);
  const open = S.current && S.current.id === sid;
  switch (ev.type) {
    case 'session_updated': upsertSession(ev.session); if (open) { S.current.agent = agentById(ev.session.agentId) || S.current.agent; renderTopbar(); renderComposerAgent(); } return;
    case 'improvement': toast(t('toast.learned', { note: ev.note }), 'learn'); return;
    case 'user_message': {
      upsertSession({ id: sid, updatedAt: ev.message.createdAt }, { running: true });
      if (open) {
        const msgs = S.current.messages;
        const pi = msgs.findIndex((m) => m.pending);
        if (pi >= 0) msgs.splice(pi, 1, ev.message); else msgs.push(ev.message);
        thread.rebuild(); thread.scrollToBottom(true);
      }
      return;
    }
    case 'turn_start': {
      const l = liveFor(sid, true); Object.assign(l, { turnId: ev.turnId, agent: ev.agent, text: '', reasoning: '', tools: [], status: '', error: null, stopReason: null });
      upsertSession({ id: sid }, { running: true });
      if (open) { S.current.running = true; S.current.lastError = null; thread.renderTail(); composer.syncSendState(); }
      return;
    }
    case 'delta': { const l = liveFor(sid, true); l.text += ev.text; if (open) thread.scheduleLive(); return; }
    case 'reasoning_delta': { const l = liveFor(sid, true); l.reasoning += ev.text; if (open) thread.scheduleLive(); return; }
    case 'status': { const l = liveFor(sid, true); l.status = ev.text; if (open) thread.scheduleLive(); return; }
    case 'tool_call': {
      const l = liveFor(sid, true);
      l.tools.push({ id: ev.id, name: ev.name, args: ev.args, result: null, isError: false, done: false, persisted: false });
      if (open) thread.renderTail();
      return;
    }
    case 'tool_result': {
      const l = liveFor(sid, true);
      let tcall = l.tools.find((x) => x.id === ev.id);
      if (!tcall) { tcall = { id: ev.id, name: ev.name, args: null, persisted: false }; l.tools.push(tcall); }
      Object.assign(tcall, { result: ev.content, isError: !!ev.isError, done: true });
      if (open) thread.renderTail();
      return;
    }
    case 'message': {
      const l = liveFor(sid, true);
      const parts = ev.message.parts || [];
      if (ev.message.role === 'assistant') { l.text = ''; l.reasoning = ''; l.status = ''; }
      for (const p of parts) { if (p.type === 'tool_call' || p.type === 'tool_result') { const x = l.tools.find((y) => y.id === p.id); if (x) x.persisted = true; } }
      if (open) { S.current.messages.push(ev.message); thread.renderTail(); }
      return;
    }
    case 'turn_end': {
      const l = liveFor(sid, true);
      l.stopReason = ev.stopReason; l.error = ev.error || null;
      upsertSession({ id: sid, updatedAt: Date.now() }, { running: false });
      if (open) { S.current.running = false; S.current.lastError = ev.error || null; S.current.lastStop = ev.stopReason; thread.renderTail(); composer.syncSendState(); }
      S.live.delete(sid);
      if (open && (!S.current.session.title) ) openSession(sid, { force: true, quiet: true });
      return;
    }
    default: return;
  }
}

/* ============================================================ router */

function parseHash() {
  const raw = location.hash.replace(/^#/, '') || '/';
  const [path, qs] = raw.split('?');
  const query = {};
  if (qs) for (const kv of qs.split('&')) { const [k, v] = kv.split('='); if (k) query[decodeURIComponent(k)] = decodeURIComponent((v || '').replace(/\+/g, ' ')); }
  const seg = path.split('/').filter(Boolean);
  if (seg[0] === 's' && seg[1]) return { name: 'chat', id: seg[1], query };
  if (seg[0] === 'settings') return { name: 'settings', page: seg[1] || 'overview', query };
  return { name: 'chat', id: null, query };
}
function navigate(hash) { if (location.hash === hash) onRoute(); else location.hash = hash; }
async function onRoute() {
  S.route = parseHash();
  S.sidebarOpen = false; syncSidebarClass();
  if (!S.auth.authenticated) return;
  if (S.route.name === 'settings') { settings.show(S.route.page, S.route.query); }
  else {
    settings.hide();
    if (S.route.id) { if (!S.current || S.current.id !== S.route.id) await openSession(S.route.id); else thread.show(); }
    else { S.current = null; thread.show(); }
    renderTopbar(); composer.reset(); renderSidebarList();
  }
}
window.addEventListener('hashchange', onRoute);

async function openSession(id, opts) {
  opts = opts || {};
  if (!opts.quiet) thread.showLoading();
  try {
    const data = await api.get('/api/sessions/' + encodeURIComponent(id));
    const prev = S.current && S.current.id === id ? S.current : null;
    S.current = { id, session: data.session, messages: data.messages || [], agent: data.agent || agentById(data.session.agentId), running: !!data.running, lastError: prev ? prev.lastError : null, lastStop: prev ? prev.lastStop : null };
    if (!data.running) S.live.delete(id);
    upsertSession(data.session, { running: !!data.running });
    thread.show(); thread.rebuild(); thread.scrollToBottom(true);
    renderTopbar(); composer.syncSendState(); composer.reset();
  } catch (e) {
    if (e.status === 404) { S.current = null; toast(t('thread.loadFailed'), 'err'); navigate('#/'); return; }
    thread.showError(e.message, () => openSession(id, opts));
  }
}

async function newSession(agentId) {
  const body = {}; const aid = agentId || S.pendingAgentId; if (aid) body.agentId = aid;
  const sess = await api.post('/api/sessions', body);
  S.pendingAgentId = null;
  S.current = { id: sess.id, session: sess, messages: [], agent: agentById(sess.agentId) || defaultAgent(), running: false };
  upsertSession(sess, { running: false });
  if (location.hash !== '#/s/' + sess.id) { location.hash = '#/s/' + sess.id; }
  thread.show(); thread.rebuild(); renderTopbar(); composer.syncSendState();
  return sess;
}

/* ============================================================ views: shell */

function renderRoot() {
  const root = $('#app');
  clear(root);
  root.className = '';
  if (S.auth.required && !S.auth.authenticated) { root.appendChild(loginView()); return; }
  root.appendChild(sidebarView());
  root.appendChild(h('div', { class: 'sidebar-backdrop', onClick: () => { S.sidebarOpen = false; syncSidebarClass(); } }));
  root.appendChild(h('div', { class: 'main' }, topbarView(), thread.view(), settings.view()));
  syncSidebarClass();
  renderSidebarList(); renderTopbar();
  ws.setStatus(S.ws);
}
function syncSidebarClass() {
  const root = $('#app'); if (!root) return;
  root.classList.toggle('sidebar-collapsed', S.sidebarCollapsed);
  root.classList.toggle('sidebar-open', S.sidebarOpen);
  const b = $('#menu-btn'); if (b) b.setAttribute('aria-expanded', String(S.sidebarOpen || !S.sidebarCollapsed));
}
function toggleSidebar() {
  if (window.innerWidth < 900) { S.sidebarOpen = !S.sidebarOpen; }
  else { S.sidebarCollapsed = !S.sidebarCollapsed; try { localStorage.setItem('kongming.sidebar', S.sidebarCollapsed ? 'collapsed' : 'open'); } catch (e) { /* ignore */ } }
  syncSidebarClass();
}

function loginView() {
  const err = h('div', { class: 'err', role: 'alert' });
  const pw = h('input', { class: 'field', type: 'password', name: 'password', autocomplete: 'current-password', placeholder: t('login.password'), 'aria-label': t('login.password'), required: true });
  const form = h('form', { onSubmit: async (e) => {
    e.preventDefault(); err.textContent = '';
    try { await api.post('/api/auth/login', { password: pw.value }); S.auth.authenticated = true; await boot(true); }
    catch (ex) { err.textContent = ex.status === 401 ? t('login.wrong') : ex.message; pw.select(); }
  } }, pw, h('button', { class: 'btn primary', type: 'submit' }, t('login.submit')), err);
  setTimeout(() => pw.focus(), 0);
  return h('div', { class: 'login' }, h('div', { class: 'login-card' }, h('img', { src: '/logo.svg', alt: '' }), h('h1', {}, t('login.title')), h('p', {}, t('login.desc')), form));
}

/* ---------------------------------------------------------------- sidebar */

function sidebarView() {
  return h('aside', { class: 'sidebar', 'aria-label': 'Sidebar' },
    h('div', { class: 'sidebar-head' },
      h('a', { class: 'brand', href: '#/', 'aria-label': t('app.name'), style: 'text-decoration:none' }, h('img', { src: '/logo.svg', alt: '' }), h('span', {}, t('app.name'))),
      h('span', { class: 'grow' }),
      h('button', { class: 'icon-btn quiet', type: 'button', title: t('nav.closeSidebar'), 'aria-label': t('nav.closeSidebar'), onClick: toggleSidebar }, icon('panel'))),
    h('nav', { class: 'sidebar-nav' },
      h('a', { class: 'nav-item newchat', href: '#/', onClick: (e) => { e.preventDefault(); S.pendingAgentId = null; S.current = null; navigate('#/'); setTimeout(() => composer.focus(), 0); } }, icon('compose'), h('span', {}, t('nav.newChat')), h('kbd', {}, '⌘⇧O')),
      h('a', { class: 'nav-item quiet', href: '#/settings/agents' }, icon('users'), h('span', {}, t('nav.agents'))),
      h('a', { class: 'nav-item quiet', href: '#/settings/skills' }, icon('skill'), h('span', {}, t('nav.skills'))),
      h('a', { class: 'nav-item quiet', href: '#/settings/automations' }, icon('clock'), h('span', {}, t('nav.automations')))),
    h('div', { class: 'session-list', id: 'session-list', role: 'navigation', 'aria-label': 'Conversations' }),
    h('div', { class: 'sidebar-foot' },
      h('a', { class: 'nav-item quiet', href: '#/settings/overview' }, icon('settings'), h('span', {}, t('nav.settings'))),
      h('button', { class: 'nav-item quiet', type: 'button', id: 'theme-btn', onClick: () => { const order = ['system', 'light', 'dark']; theme.set(order[(order.indexOf(theme.get()) + 1) % 3]); renderThemeBtn(); } })));
}
function renderThemeBtn() {
  const b = $('#theme-btn'); if (!b) return; clear(b);
  const m = theme.get();
  append(b, [icon(m === 'dark' ? 'moon' : m === 'light' ? 'sun' : 'palette'), h('span', {}, `${t('theme.title')}: ${t('theme.' + m)}`)]);
}

function renderSidebarList() {
  const list = $('#session-list'); if (!list) return;
  clear(list);
  if (!S.sessions.length) { list.appendChild(h('div', { class: 'sidebar-empty' }, t('session.empty'))); return; }
  const today = dayKey(Date.now()), yesterday = dayKey(Date.now() - 86400000);
  const groups = [['group.today', []], ['group.yesterday', []], ['group.earlier', []]];
  for (const s of S.sessions) { const k = dayKey(s.updatedAt || s.createdAt || 0); (k === today ? groups[0] : k === yesterday ? groups[1] : groups[2])[1].push(s); }
  for (const [label, items] of groups) {
    if (!items.length) continue;
    const sec = h('section', { 'aria-label': t(label) }, h('div', { class: 'session-group-label' }, t(label)));
    const ul = h('ul', { style: 'list-style:none;margin:0;padding:0' });
    for (const s of items) ul.appendChild(h('li', {}, sessionRow(s)));
    sec.appendChild(ul); list.appendChild(sec);
  }
}
function sessionRow(s) {
  const active = S.current && S.current.id === s.id && S.route.name === 'chat';
  const title = h('span', { class: 'title' }, s.title || t('session.untitled'));
  const row = h('div', { class: 'session-row' + (active ? ' active' : ''), role: 'link', tabindex: 0, 'aria-current': active ? 'page' : null,
    onClick: () => navigate('#/s/' + s.id), onKeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); navigate('#/s/' + s.id); } } });
  if (s.running) row.appendChild(h('span', { class: 'dot-live', title: t('session.running'), 'aria-label': t('session.running') }));
  row.appendChild(title);
  if (s.agentHandle) row.appendChild(h('span', { class: 'handle', style: `--h:${hue(s.agentHandle)}`, title: '@' + s.agentHandle }, '@' + s.agentHandle));
  const acts = h('span', { class: 'actions' },
    h('button', { class: 'icon-btn quiet', type: 'button', title: t('session.rename'), 'aria-label': t('session.rename'), onClick: (e) => { e.stopPropagation(); startRename(row, title, s); } }, icon('pencil')),
    h('button', { class: 'icon-btn quiet', type: 'button', title: t('session.delete'), 'aria-label': t('session.delete'), onClick: (e) => { e.stopPropagation(); confirmDeleteSession(s); } }, icon('trash')));
  row.appendChild(acts);
  return row;
}
function startRename(row, titleEl, s) {
  const input = h('input', { class: 'rename', type: 'text', value: s.title || '', 'aria-label': t('session.rename'), onClick: (e) => e.stopPropagation(), onKeydown: (e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); done(true); } if (e.key === 'Escape') done(false); } });
  let finished = false;
  async function done(save) {
    if (finished) return; finished = true;
    const v = input.value.trim();
    input.replaceWith(titleEl);
    if (save && v && v !== s.title) { try { const upd = await api.patch('/api/sessions/' + s.id, { title: v }); upsertSession(upd); renderTopbar(); } catch (e) { toast(e.message, 'err'); } }
  }
  input.addEventListener('blur', () => done(true));
  titleEl.replaceWith(input); input.focus(); input.select();
}
function confirmDeleteSession(s) {
  modal.confirm({ title: t('session.deleteTitle'), desc: t('session.deleteDesc'), confirm: t('common.delete'), danger: true, onConfirm: async () => {
    await api.del('/api/sessions/' + s.id);
    S.sessions = S.sessions.filter((x) => x.id !== s.id); S.live.delete(s.id);
    if (S.current && S.current.id === s.id) { S.current = null; navigate('#/'); } else renderSidebarList();
  } });
}

/* ---------------------------------------------------------------- topbar */

function topbarView() {
  return h('header', { class: 'topbar' },
    h('button', { class: 'icon-btn quiet', id: 'menu-btn', type: 'button', title: t('nav.toggleSidebar'), 'aria-label': t('nav.toggleSidebar'), 'aria-expanded': 'true', onClick: toggleSidebar }, icon('menu')),
    h('div', { class: 'title', id: 'topbar-title' }, h('span', {}, '')),
    h('span', { class: 'spacer' }),
    h('span', { class: 'conn-badge', id: 'conn-badge' }, ''),
    h('span', { id: 'topbar-agent' }));
}
function renderTopbar() {
  const tt = $('#topbar-title'); const ta = $('#topbar-agent'); if (!tt || !ta) return;
  if (S.route.name === 'settings') { tt.firstChild.textContent = t('settings.title'); clear(ta); return; }
  const sess = S.current && S.current.session;
  tt.firstChild.textContent = sess ? (sess.title || t('session.untitled')) : t('nav.newChat');
  tt.title = sess ? t('session.info', { channel: sess.channel, count: S.current.messages.length }) : '';
  clear(ta); ta.appendChild(agentPill());
}
function agentPill() {
  const a = sessionAgent();
  const pill = h('button', { class: 'agent-pill quiet', type: 'button', title: t('composer.pickAgent'), 'aria-haspopup': 'listbox', 'aria-expanded': 'false' },
    a ? h('span', { class: 'avatar', style: `--h:${hue(a.handle)}`, 'aria-hidden': 'true' }, a.handle.slice(0, 2)) : icon('bot'),
    h('span', {}, a ? '@' + a.handle : t('composer.agent')), icon('chev-down'));
  pill.addEventListener('click', () => agentPicker(pill));
  return pill;
}
function agentPicker(anchor) {
  const cur = sessionAgent();
  popover.open(anchor, {
    items: S.agents.map((a) => ({ key: a.id, title: '@' + a.handle, sub: [a.name, a.role].filter(Boolean).join(' · '), avatar: a.handle, selected: cur && cur.id === a.id })),
    label: t('composer.pickAgent'),
    onSelect: async (key) => {
      if (S.current) {
        try { const sess = await api.patch('/api/sessions/' + S.current.id, { agentId: key }); S.current.agent = agentById(key); upsertSession(sess); }
        catch (e) { toast(e.message, 'err'); return; }
      } else S.pendingAgentId = key;
      renderTopbar(); renderComposerAgent(); thread.renderHero();
    },
  });
}
function renderComposerAgent() { const el = $('#composer-agent'); if (el) { clear(el); el.appendChild(agentPill()); } composer.updatePlaceholder(); }

/* ============================================================ views: thread */

const thread = (() => {
  let viewEl, viewport, col, tailEl, heroEl, stick = true, raf = 0, lastLiveRender = 0;
  const openKeys = new Set();

  function view() {
    viewport = h('div', { class: 'thread-viewport', onScroll: () => { stick = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 80; } });
    viewport.addEventListener('click', onThreadClick);
    col = h('div', { class: 'thread-col' });
    viewport.appendChild(col);
    viewEl = h('section', { id: 'view-thread', class: 'thread', 'aria-label': 'Conversation' }, viewport, composer.view());
    return viewEl;
  }
  function onThreadClick(e) {
    const copyBtn = e.target.closest('[data-copy]');
    if (copyBtn) { const code = copyBtn.closest('pre').querySelector('code'); copyText(code ? code.textContent : ''); copyBtn.textContent = t('common.copied'); setTimeout(() => { copyBtn.textContent = t('common.copy'); }, 1200); return; }
    const tog = e.target.closest('[data-toggle]');
    if (tog) { const box = tog.closest('[data-key]'); const key = box.dataset.key; const open = box.dataset.open !== 'true'; box.dataset.open = String(open); tog.setAttribute('aria-expanded', String(open)); if (open) openKeys.add(key); else openKeys.delete(key); }
  }
  function show() { if (!viewEl) return; viewEl.hidden = false; const sv = $('#view-settings'); if (sv) sv.hidden = true; if (!S.current) rebuild(); }
  function showLoading() { show(); clear(col); col.appendChild(h('div', { class: 'status-line', style: 'margin:2rem auto;justify-content:center' }, icon('spinner', 'spin'), t('common.loading'))); viewEl.dataset.layout = 'thread'; }
  function showError(msg, retry) { clear(col); col.appendChild(h('div', { class: 'error-banner' }, icon('alert'), h('span', { class: 'grow' }, msg), h('button', { type: 'button', onClick: retry }, t('common.retry')))); }

  function renderHero() {
    if (!heroEl) return;
    clear(heroEl);
    const a = sessionAgent();
    append(heroEl, [h('img', { src: '/logo.svg', alt: '' }), h('h1', {}, t('greeting.' + S.greeting)), h('p', {}, t('hero.sub', { agent: a ? '@' + a.handle : t('app.name') })),
      h('div', { class: 'quick-actions' }, ['plan', 'skills', 'team'].map((k) => h('button', { type: 'button', onClick: () => composer.sendText(t(`hero.quick.${k}Prompt`)) }, t('hero.quick.' + k))))]);
  }

  /** Full render of the open session (or the hero when none). */
  function rebuild() {
    clear(col); tailEl = null; heroEl = null;
    if (!S.current || !S.current.messages.length) {
      if (!S.current) { viewEl.dataset.layout = 'hero'; heroEl = h('div', { class: 'hero' }); col.appendChild(heroEl); renderHero(); return; }
    }
    viewEl.dataset.layout = 'thread';
    const msgs = S.current.messages;
    let i = 0;
    while (i < msgs.length) {
      const m = msgs[i];
      if (m.role === 'user' || m.role === 'system') { col.appendChild(userRow(m)); i++; continue; }
      const group = [];
      while (i < msgs.length && msgs[i].role !== 'user' && msgs[i].role !== 'system') group.push(msgs[i++]);
      if (i < msgs.length) col.appendChild(assistantBlock(group, null, false));
      else { tailEl = assistantBlock(group, liveFor(S.current.id), true); col.appendChild(tailEl); }
    }
    if (!tailEl) { tailEl = assistantBlock([], liveFor(S.current.id), true); col.appendChild(tailEl); }
    col.appendChild(h('div', { class: 'thread-end-gap' }));
  }

  /** Re-render just the last assistant block (persisted tail rounds + live state). */
  function renderTail() {
    if (!S.current || !col) return;
    const msgs = S.current.messages;
    let j = msgs.length; while (j > 0 && msgs[j - 1].role !== 'user' && msgs[j - 1].role !== 'system') j--;
    const group = msgs.slice(j);
    if (!tailEl || !tailEl.isConnected) { rebuild(); return; }
    const fresh = assistantBlock(group, liveFor(S.current.id), true);
    tailEl.replaceWith(fresh); tailEl = fresh;
    if (stick) scrollToBottom();
  }

  function scheduleLive() {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      const now = performance.now();
      if (now - lastLiveRender < 60) { raf = requestAnimationFrame(() => { raf = 0; scheduleLive(); }); return; }
      lastLiveRender = now;
      const l = S.current && liveFor(S.current.id); if (!l || !tailEl) return;
      const txt = tailEl.querySelector('[data-live-text]'); const rs = tailEl.querySelector('[data-live-reasoning]'); const st = tailEl.querySelector('[data-live-status]');
      if (!txt && (l.text || l.reasoning)) { renderTail(); return; }
      if (txt) { txt.hidden = !l.text; if (l.text) txt.innerHTML = md.render(l.text); }
      if (rs) { const box = rs.closest('.activity'); box.hidden = !l.reasoning; rs.textContent = l.reasoning; const hd = box.querySelector('.activity-head'); if (hd && l.reasoning) { hd.querySelector('.sheen').dataset.sheenText = t('thread.thinkingLive'); } }
      if (st) { st.hidden = !l.status || !!l.text; st.lastChild.textContent = l.status; const sh = st.querySelector('.sheen'); if (sh) sh.dataset.sheenText = l.status; }
      if (stick) scrollToBottom();
    });
  }
  function scrollToBottom(force) { if (!viewport) return; if (force) stick = true; viewport.scrollTop = viewport.scrollHeight; }

  function userRow(m) {
    const text = (m.parts || []).filter((p) => p.type === 'text').map((p) => p.text).join('\n');
    const bubble = h('div', { class: 'user-bubble' }, mentionSpans(text));
    const meta = h('div', { class: 'msg-meta' }, m.pending ? [icon('spinner', 'spin'), t('thread.sending')] : m.failed ? [h('span', { class: 'danger-text' }, t('thread.notSent')), h('button', { class: 'btn xs ghost', type: 'button', onClick: () => composer.retry(m) }, t('common.retry'))] : [fmtTime(m.createdAt),
      h('button', { class: 'icon-btn quiet', type: 'button', style: 'width:1.5rem;height:1.5rem', title: t('thread.copyMessage'), 'aria-label': t('thread.copyMessage'), onClick: () => copyText(text) }, icon('copy'))]);
    return h('div', { class: 'msg-row', 'data-role': 'user' }, bubble, meta);
  }
  function mentionSpans(text) {
    const out = []; const re = /(^|[\s(\[,:;])(@(?:skill|session|mcp):[A-Za-z0-9_./-]+|@[A-Za-z0-9_-]+)/g; let last = 0, m;
    while ((m = re.exec(text))) { const start = m.index + m[1].length; out.push(text.slice(last, start)); out.push(h('span', { class: 'mention' + (m[2].includes(':') ? ' kind' : '') }, m[2])); last = start + m[2].length; }
    out.push(text.slice(last)); return out;
  }

  /** Build the segments of one assistant round-group: thinking, text, tool clusters, plus live state. */
  function assistantBlock(group, live, isTail) {
    const block = h('div', { class: 'msg-row assistant-block', 'data-role': 'assistant' });
    const results = new Map();
    for (const m of group) for (const p of m.parts || []) if (p.type === 'tool_result') results.set(p.id, p);
    const segs = [];
    const pushTool = (tool) => { const lastSeg = segs[segs.length - 1]; if (lastSeg && lastSeg.type === 'tools') lastSeg.tools.push(tool); else segs.push({ type: 'tools', tools: [tool] }); };
    let agent = null;
    for (const m of group) {
      if (!agent && m.agentId) agent = agentById(m.agentId);
      for (const p of m.parts || []) {
        if (p.type === 'thinking' && p.text) segs.push({ type: 'thinking', text: p.text, key: 'th-' + m.id });
        else if (p.type === 'text' && p.text) segs.push({ type: 'text', text: p.text });
        else if (p.type === 'tool_call') {
          let r = results.get(p.id); let done = !!r, isError = r ? !!r.isError : false, content = r ? r.content : null;
          if (!r && live) { const lt = live.tools.find((x) => x.id === p.id); if (lt && lt.done) { done = true; isError = lt.isError; content = lt.result; } }
          pushTool({ id: p.id, name: p.name, args: p.args, done, isError, content });
        }
      }
    }
    const running = isTail && S.current && S.current.running;
    if (live && isTail) {
      if (!agent && live.agent) agent = live.agent;
      for (const lt of live.tools) if (!lt.persisted) pushTool({ id: lt.id, name: lt.name, args: lt.args, done: lt.done, isError: lt.isError, content: lt.result });
    }
    if (agent) block.appendChild(h('div', { class: 'agent-tag' }, h('span', { class: 'avatar', style: `--h:${hue(agent.handle)}`, 'aria-hidden': 'true' }, agent.handle.slice(0, 2)), h('span', {}, agent.name || '@' + agent.handle)));
    for (const s of segs) {
      if (s.type === 'thinking') block.appendChild(thinkingBox(s.key, s.text, false));
      else if (s.type === 'text') block.appendChild(h('div', { class: 'md', html: md.render(s.text) }));
      else block.appendChild(toolCluster(s.tools, running && s === segs[segs.length - 1] && s.tools.some((x) => !x.done)));
    }
    if (isTail && running) {
      const l = live || {};
      const rbox = thinkingBox('th-live', l.reasoning || '', true); rbox.hidden = !l.reasoning; block.appendChild(rbox);
      const txt = h('div', { class: 'md', 'data-live-text': '' }); if (l.text) txt.innerHTML = md.render(l.text); else txt.hidden = true; block.appendChild(txt);
      const label = l.status || t('thread.working');
      const st = h('div', { class: 'status-line', 'data-live-status': '' }, icon('activity'), h('span', { class: 'sheen', 'data-sheen-text': label }, label));
      st.hidden = !!l.text && !l.status; block.appendChild(st);
    }
    if (isTail && !running && S.current) {
      if (S.current.lastError) block.appendChild(h('div', { class: 'error-banner', role: 'alert' }, icon('alert'), h('span', { class: 'grow' }, S.current.lastError), h('button', { type: 'button', onClick: () => composer.retryLast() }, t('common.retry'))));
      else if (S.current.lastStop === 'cancelled' || S.current.lastStop === 'stopped') block.appendChild(h('div', { class: 'sys-note' }, t('thread.stopped')));
    }
    return block;
  }
  function thinkingBox(key, text, live) {
    const open = live ? !openKeys.has('closed:' + key) : openKeys.has(key);
    const label = live ? t('thread.thinkingLive') : t('thread.thinking');
    const box = h('div', { class: 'activity', 'data-key': key, 'data-open': String(open) },
      h('button', { class: 'activity-head quiet' + (live ? ' live' : ''), type: 'button', 'data-toggle': '', 'aria-expanded': String(open) }, icon('chev', 'chev'), icon('brain', 'act-icon'), live ? h('span', { class: 'sheen', 'data-sheen-text': label }, label) : h('span', {}, label)),
      h('div', { class: 'activity-body' }, h('div', { class: 'thinking-text', 'data-live-reasoning': live ? '' : null }, text)));
    if (live) box.querySelector('[data-toggle]').addEventListener('click', () => { if (box.dataset.open === 'true') openKeys.delete('closed:' + key); else openKeys.add('closed:' + key); });
    return box;
  }
  function toolCluster(tools, active) {
    const key = 'tc-' + tools[0].id;
    const open = openKeys.has(key) || (active && openKeys.has(key));
    const pendingTool = tools.find((x) => !x.done);
    let label;
    if (active && pendingTool) label = tools.length > 1 ? t('thread.toolsLive', { count: tools.length }) : t('thread.toolLive', { name: pendingTool.name });
    else label = tools.length === 1 ? t('thread.toolOne', { name: tools[0].name }) : t('thread.toolMany', { count: tools.length });
    const head = h('button', { class: 'activity-head quiet' + (active ? ' live' : ''), type: 'button', 'data-toggle': '', 'aria-expanded': String(open) }, icon('chev', 'chev'), icon('tool', 'act-icon'), active ? h('span', { class: 'sheen', 'data-sheen-text': label }, label) : h('span', {}, label));
    const body = h('div', { class: 'activity-body' }, tools.map(toolRow));
    return h('div', { class: 'activity', 'data-key': key, 'data-open': String(open) }, head, body);
  }
  function toolRow(tl) {
    const key = 'tr-' + tl.id; const open = openKeys.has(key);
    const status = !tl.done ? h('span', { class: 'status pending' }, t('thread.pending')) : tl.isError ? h('span', { class: 'status err' }, t('thread.error')) : h('span', { class: 'status' }, icon('check'));
    const head = h('button', { class: 'tool-row-head', type: 'button', 'data-toggle': '', 'aria-expanded': String(open) }, icon('chev', 'chev'), h('span', { class: 'name' }, prettyTool(tl.name)), h('span', { class: 'summary' }, argSummary(tl.args)), status);
    const body = h('div', { class: 'tool-row-body' }, h('h5', {}, t('thread.args')), h('pre', {}, safeJson(tl.args == null ? {} : tl.args)),
      tl.done ? [h('h5', {}, t('thread.result')), h('pre', { class: tl.isError ? 'err' : '' }, String(tl.content == null ? '' : tl.content).slice(0, 20000))] : null);
    return h('div', { class: 'tool-row', 'data-key': key, 'data-open': String(open) }, head, body);
  }
  function prettyTool(name) { const m = /^mcp__([^_].*?)__(.+)$/.exec(name || ''); return m ? `${m[1]} › ${m[2]}` : name; }
  return { view, show, showLoading, showError, rebuild, renderTail, renderHero, scheduleLive, scrollToBottom };
})();

/* ============================================================ views: composer + palettes */

const SLASH_COMMANDS = [
  { cmd: '/new', desc: 'cmd.new' }, { cmd: '/clear', desc: 'cmd.clear' }, { cmd: '/agent', desc: 'cmd.agent', arg: '<handle>' }, { cmd: '/stop', desc: 'cmd.stop' }, { cmd: '/help', desc: 'cmd.help' },
];

const composer = (() => {
  let root, ta, sendBtn, paletteEl, metaEl;
  let pal = null; // { kind: 'mention' | 'slash', items, index, start, end }
  let lastSent = null;

  function view() {
    ta = h('textarea', { rows: 1, placeholder: t('composer.placeholderGeneric'), 'aria-label': t('composer.send'), autocomplete: 'off', autocapitalize: 'sentences', spellcheck: 'true', 'aria-autocomplete': 'list', 'aria-expanded': 'false', 'aria-controls': 'composer-palette', enterkeyhint: 'send' });
    ta.addEventListener('input', onInput);
    ta.addEventListener('keydown', onKeydown);
    ta.addEventListener('click', detectPalette);
    ta.addEventListener('blur', () => setTimeout(() => { if (pal && !paletteEl.contains(document.activeElement)) closePalette(); }, 120));
    sendBtn = h('button', { class: 'send-btn', type: 'button', title: t('composer.send'), 'aria-label': t('composer.send'), onClick: () => (S.current && S.current.running ? stop() : send()) }, icon('send'));
    metaEl = h('span', { class: 'meta', 'aria-live': 'off' });
    paletteEl = h('div', { class: 'palette', id: 'composer-palette', role: 'listbox', hidden: true });
    root = h('div', { class: 'composer' }, paletteEl,
      h('div', { class: 'composer-card' }, ta,
        h('div', { class: 'composer-foot' },
          h('button', { class: 'composer-action', type: 'button', title: t('composer.attach'), 'aria-label': t('composer.attach'), disabled: true }, icon('paperclip')),
          h('button', { class: 'composer-action', type: 'button', title: t('composer.mention'), 'aria-label': t('composer.mention'), onClick: () => insertAtCaret('@') }, icon('at')),
          h('button', { class: 'composer-action', type: 'button', title: t('composer.commands'), 'aria-label': t('composer.commands'), onClick: () => { if (!ta.value.trim()) { ta.value = '/'; onInput(); ta.focus(); } else insertAtCaret('/'); } }, icon('slash')),
          h('span', { id: 'composer-agent' }, agentPill()),
          h('span', { class: 'grow' }), metaEl, sendBtn)),
      h('div', { class: 'composer-hint' }, t('composer.hint')));
    return h('div', { class: 'composer-dock' }, root);
  }
  function autosize() { ta.style.height = 'auto'; ta.style.height = Math.min(ta.scrollHeight, Math.floor(window.innerHeight * 0.4)) + 'px'; }
  function onInput() { autosize(); syncSendState(); detectPalette(); const n = ta.value.length; metaEl.textContent = n > 400 ? t('composer.chars', { count: n }) : ''; }
  function onKeydown(e) {
    if (pal) {
      if (e.key === 'ArrowDown') { e.preventDefault(); move(1); return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); return; }
      if ((e.key === 'Enter' || e.key === 'Tab') && pal.items.length) { e.preventDefault(); selectItem(pal.index); return; }
      if (e.key === 'Escape') { e.preventDefault(); closePalette(); return; }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.altKey && !e.ctrlKey && !e.metaKey) { e.preventDefault(); send(); return; }
    if (e.key === 'Escape' && S.current && S.current.running) stop();
  }
  function focus() { if (ta) ta.focus(); }
  function updatePlaceholder() { if (!ta) return; const a = sessionAgent(); ta.placeholder = a ? t('composer.placeholder', { agent: '@' + a.handle }) : t('composer.placeholderGeneric'); }
  function reset() { if (!ta) return; closePalette(); updatePlaceholder(); syncSendState(); renderComposerAgent(); }
  function syncSendState() {
    if (!sendBtn) return;
    const running = !!(S.current && S.current.running);
    sendBtn.classList.toggle('stop', running);
    clear(sendBtn); sendBtn.appendChild(icon(running ? 'stop' : 'send'));
    sendBtn.title = sendBtn.ariaLabel = running ? t('composer.stop') : t('composer.send');
    sendBtn.disabled = !running && !ta.value.trim();
  }
  function insertAtCaret(text) {
    const s = ta.selectionStart, e = ta.selectionEnd; const before = ta.value.slice(0, s);
    const pad = before && !/\s$/.test(before) && text === '@' ? ' ' : '';
    ta.setRangeText(pad + text, s, e, 'end'); ta.focus(); onInput();
  }

  async function send() {
    const text = ta.value.trim();
    if (!text) return;
    if (S.current && S.current.running) return;
    closePalette();
    const slash = /^\/([a-z]+)(?:\s+(.*))?$/is.exec(text);
    if (slash) { ta.value = ''; onInput(); await runSlash(slash[1].toLowerCase(), (slash[2] || '').trim()); return; }
    ta.value = ''; onInput();
    try {
      if (!S.current) await newSession();
    } catch (e) { toast(e.message, 'err'); ta.value = text; onInput(); return; }
    const sid = S.current.id;
    const optimistic = { id: 'local-' + uid(), role: 'user', parts: [{ type: 'text', text }], createdAt: Date.now(), pending: true };
    S.current.messages.push(optimistic); S.current.lastError = null; S.current.lastStop = null;
    thread.rebuild(); thread.scrollToBottom(true);
    lastSent = text;
    try {
      await ws.send({ type: 'send', sessionId: sid, text });
    } catch (e) {
      // Fall back to HTTP when the socket is down; the reply still streams on the bus.
      try { await api.post('/api/sessions/' + sid + '/messages', { text }); }
      catch (e2) { optimistic.pending = false; optimistic.failed = true; if (S.current && S.current.id === sid) thread.rebuild(); toast(e2.message || e.message, 'err'); }
    }
  }
  function sendText(text) { ta.value = text; onInput(); send(); }
  function retry(m) { if (!S.current) return; S.current.messages = S.current.messages.filter((x) => x !== m); sendText((m.parts || []).map((p) => p.text || '').join('\n')); }
  function retryLast() { if (!S.current) return; const last = [...S.current.messages].reverse().find((m) => m.role === 'user'); const text = last ? (last.parts || []).filter((p) => p.type === 'text').map((p) => p.text).join('\n') : lastSent; if (text) sendText(text); }
  async function stop() {
    if (!S.current) return; const sid = S.current.id;
    try { await ws.send({ type: 'stop', sessionId: sid }); } catch (e) { try { await api.post('/api/sessions/' + sid + '/stop'); } catch (e2) { toast(e2.message, 'err'); } }
  }

  async function runSlash(cmd, arg) {
    switch (cmd) {
      case 'new': S.pendingAgentId = null; S.current = null; navigate('#/'); focus(); return;
      case 'clear': {
        if (!S.current) { toast(t('cmd.needSession'), 'err'); return; }
        modal.confirm({ title: t('session.clearTitle'), desc: t('session.clearDesc'), confirm: t('common.confirm'), danger: true, onConfirm: async () => {
          await api.post('/api/sessions/' + S.current.id + '/clear'); S.current.messages = []; S.current.lastError = null; thread.rebuild(); toast(t('thread.cleared'));
        } });
        return;
      }
      case 'agent': {
        const handle = arg.replace(/^@/, '').toLowerCase();
        if (!handle) { toast(t('cmd.agentUsage'), 'err'); return; }
        const a = agentByHandle(handle); if (!a) { toast(t('cmd.noAgent', { handle }), 'err'); return; }
        if (S.current) { try { const sess = await api.patch('/api/sessions/' + S.current.id, { agentId: a.id }); S.current.agent = a; upsertSession(sess); } catch (e) { toast(e.message, 'err'); return; } }
        else S.pendingAgentId = a.id;
        renderTopbar(); renderComposerAgent(); thread.renderHero(); toast(t('cmd.switched', { handle }));
        return;
      }
      case 'stop': if (S.current && S.current.running) stop(); else toast(t('cmd.nothingToStop')); return;
      case 'help': default:
        modal.info({ title: t('palette.commands'), body: h('div', { class: 'tool-list' }, SLASH_COMMANDS.map((c) => h('div', { class: 'tl' }, h('b', {}, c.cmd + (c.arg ? ' ' + c.arg : '')), h('p', {}, t(c.desc)))), h('p', { class: 'hint mt' }, t('cmd.helpText'))) });
    }
  }

  /* ---- palettes (mention / slash) ---- */
  const MENTION_RE = /(^|[\s(\[,:;])@([A-Za-z0-9_-]*)(?::([A-Za-z0-9_./-]*))?$/;
  function detectPalette() {
    if (!ta) return;
    const caret = ta.selectionStart; const before = ta.value.slice(0, caret);
    const sm = /^\/([a-z]*)$/i.exec(before);
    if (sm && caret === ta.value.length) { openSlash(sm[1].toLowerCase()); return; }
    const mm = MENTION_RE.exec(before);
    if (mm) { openMention(mm, caret); return; }
    closePalette();
  }
  function openSlash(q) {
    const items = SLASH_COMMANDS.filter((c) => c.cmd.slice(1).startsWith(q)).map((c) => ({ group: 'palette.commands', title: c.cmd + (c.arg ? ' ' + c.arg : ''), sub: t(c.desc), insert: c.cmd + (c.arg ? ' ' : ''), replaceAll: true }));
    pal = { kind: 'slash', items, index: 0, start: 0, end: ta.value.length };
    renderPalette();
  }
  function openMention(mm, caret) {
    const word = (mm[2] || '').toLowerCase(); const kind = mm[3] !== undefined ? word : null; const val = (mm[3] || '').toLowerCase();
    const start = caret - (mm[0].length - mm[1].length);
    const items = [];
    const match = (q, ...fields) => !q || fields.some((f) => String(f || '').toLowerCase().includes(q));
    if (!kind || kind === '') {
      for (const a of S.agents) if (match(word, a.handle, a.name, a.role)) items.push({ group: 'palette.agents', title: '@' + a.handle, sub: [a.name, a.role].filter(Boolean).join(' · '), insert: '@' + a.handle + ' ', avatar: a.handle });
    }
    if (!kind || kind === 'skill') {
      for (const k of S.skills) if (k.enabled !== false && match(kind ? val : word, k.name, k.description)) items.push({ group: 'palette.skills', title: k.name, sub: k.description, token: '@skill:' + k.name, insert: '@skill:' + k.name + ' ' });
    }
    if (!kind || kind === 'session') {
      for (const s of S.sessions.slice(0, 50)) if ((!S.current || s.id !== S.current.id) && match(kind ? val : word, s.title, s.id)) items.push({ group: 'palette.sessions', title: s.title || t('session.untitled'), sub: '@' + (s.agentHandle || '') + ' · ' + relTime(s.updatedAt), token: '@session:' + s.id, insert: '@session:' + s.id + ' ' });
    }
    if (!kind || kind === 'mcp') {
      for (const m of S.mcp) if (match(kind ? val : word, m.name)) items.push({ group: 'palette.mcp', title: m.name, sub: t('mcp.tools', { count: m.toolCount || 0 }), token: '@mcp:' + m.name, insert: '@mcp:' + m.name + ' ' });
    }
    if (!kind) {
      // Structured kinds as typeable prefixes, so "@sk" discovers "@skill:".
      for (const k of ['skill', 'session', 'mcp']) if (k.startsWith(word) && word) items.push({ group: 'palette.commands', title: '@' + k + ':', sub: t('palette.' + (k === 'skill' ? 'skills' : k === 'session' ? 'sessions' : 'mcp')), insert: '@' + k + ':', keepOpen: true });
    }
    if ((!S.skills.length || !S.mcp.length) && !openMention.loading) {
      openMention.loading = true;
      Promise.allSettled([S.skills.length ? null : loadSkills(), S.mcp.length ? null : loadMcp()]).then(() => { openMention.loading = false; if (pal && pal.kind === 'mention') detectPalette(); });
    }
    const limited = items.slice(0, 40);
    pal = { kind: 'mention', items: limited, index: Math.min(pal && pal.kind === 'mention' ? pal.index : 0, Math.max(0, limited.length - 1)), start, end: caret };
    renderPalette();
  }
  function renderPalette() {
    clear(paletteEl); paletteEl.hidden = false; ta.setAttribute('aria-expanded', 'true');
    const list = h('div', { class: 'palette-list' });
    if (!pal.items.length) list.appendChild(h('div', { class: 'palette-empty' }, t('palette.empty')));
    let lastGroup = null, groupEl = null;
    pal.items.forEach((it, i) => {
      if (it.group !== lastGroup) { lastGroup = it.group; groupEl = h('div', { class: 'palette-group', role: 'group', 'aria-label': t(it.group) }, h('div', { class: 'palette-group-label' }, t(it.group))); list.appendChild(groupEl); }
      const el = h('div', { class: 'palette-item', role: 'option', id: 'pal-' + i, 'aria-selected': String(i === pal.index), onMousedown: (e) => e.preventDefault(), onClick: () => selectItem(i), onMousemove: () => { if (pal.index !== i) { pal.index = i; markSelected(); } } },
        it.avatar ? h('span', { class: 'avatar', style: `--h:${hue(it.avatar)}`, 'aria-hidden': 'true' }, it.avatar.slice(0, 2)) : icon(it.group === 'palette.skills' ? 'skill' : it.group === 'palette.sessions' ? 'compose' : it.group === 'palette.mcp' ? 'plug' : 'slash'),
        h('span', { class: 'pi-main' }, h('span', { class: 'pi-title' }, h('b', {}, it.title), it.token ? h('span', { class: 'pi-token' }, it.token) : null), it.sub ? h('span', { class: 'pi-sub' }, it.sub) : null));
      groupEl.appendChild(el);
    });
    paletteEl.appendChild(list);
    markSelected();
  }
  function markSelected() {
    paletteEl.querySelectorAll('.palette-item').forEach((el, i) => { const sel = i === pal.index; el.setAttribute('aria-selected', String(sel)); if (sel) { el.scrollIntoView({ block: 'nearest' }); ta.setAttribute('aria-activedescendant', el.id); } });
  }
  function move(d) { if (!pal || !pal.items.length) return; pal.index = (pal.index + d + pal.items.length) % pal.items.length; markSelected(); }
  function selectItem(i) {
    const it = pal && pal.items[i]; if (!it) return;
    const { start, end } = pal;
    const keepOpen = it.keepOpen;
    ta.setRangeText(it.insert, it.replaceAll ? 0 : start, it.replaceAll ? ta.value.length : end, 'end');
    ta.focus();
    if (keepOpen) { onInput(); return; }
    closePalette(); autosize(); syncSendState();
  }
  function closePalette() { pal = null; if (paletteEl) { paletteEl.hidden = true; clear(paletteEl); } if (ta) { ta.setAttribute('aria-expanded', 'false'); ta.removeAttribute('aria-activedescendant'); } }

  return { view, focus, reset, send, sendText, retry, retryLast, stop, syncSendState, updatePlaceholder };
})();

/* ============================================================ popover / modal / toast */

const popover = (() => {
  let el = null, anchorEl = null, cleanup = null;
  function close() { if (el) el.remove(); el = null; if (cleanup) cleanup(); cleanup = null; if (anchorEl) { anchorEl.setAttribute('aria-expanded', 'false'); anchorEl.focus(); } anchorEl = null; }
  function open(anchor, opts) {
    if (el) { const same = anchorEl === anchor; close(); if (same) return; }
    anchorEl = anchor; anchor.setAttribute('aria-expanded', 'true');
    let index = Math.max(0, opts.items.findIndex((i) => i.selected));
    el = h('div', { class: 'popover', role: 'listbox', 'aria-label': opts.label || '', tabindex: -1 });
    const items = opts.items.map((it, i) => h('div', { class: 'palette-item', role: 'option', 'aria-selected': String(i === index), onClick: () => { close(); opts.onSelect(it.key); } },
      it.avatar ? h('span', { class: 'avatar', style: `--h:${hue(it.avatar)}`, 'aria-hidden': 'true' }, it.avatar.slice(0, 2)) : null,
      h('span', { class: 'pi-main' }, h('span', { class: 'pi-title' }, h('b', {}, it.title)), it.sub ? h('span', { class: 'pi-sub' }, it.sub) : null), it.selected ? icon('check', 'check') : null));
    append(el, items);
    if (!items.length) el.appendChild(h('div', { class: 'palette-empty' }, t('palette.empty')));
    document.body.appendChild(el);
    const r = anchor.getBoundingClientRect(); const w = el.offsetWidth, hh = el.offsetHeight;
    let left = Math.min(Math.max(8, r.right - w), window.innerWidth - w - 8);
    let top = r.bottom + 6; if (top + hh > window.innerHeight - 8) top = Math.max(8, r.top - hh - 6);
    el.style.left = left + 'px'; el.style.top = top + 'px';
    const mark = () => items.forEach((x, i) => x.setAttribute('aria-selected', String(i === index)));
    const onKey = (e) => {
      if (e.key === 'Escape') { e.preventDefault(); close(); }
      else if (e.key === 'ArrowDown') { e.preventDefault(); index = (index + 1) % items.length; mark(); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); index = (index - 1 + items.length) % items.length; mark(); }
      else if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); const it = opts.items[index]; if (it) { close(); opts.onSelect(it.key); } }
    };
    const onDown = (e) => { if (el && !el.contains(e.target) && e.target !== anchor && !anchor.contains(e.target)) close(); };
    el.addEventListener('keydown', onKey);
    setTimeout(() => { document.addEventListener('mousedown', onDown); window.addEventListener('resize', close, { once: true }); }, 0);
    cleanup = () => { document.removeEventListener('mousedown', onDown); };
    el.focus();
  }
  return { open, close };
})();

const modal = (() => {
  let overlay = null, prevFocus = null;
  function close() { if (!overlay) return; overlay.remove(); overlay = null; document.removeEventListener('keydown', onKey); if (prevFocus && prevFocus.focus) prevFocus.focus(); prevFocus = null; }
  function onKey(e) {
    if (e.key === 'Escape') { e.preventDefault(); close(); return; }
    if (e.key === 'Tab' && overlay) {
      const f = [...overlay.querySelectorAll('button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])')];
      if (!f.length) return;
      const first = f[0], last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  }
  function open(opts) {
    close(); prevFocus = document.activeElement;
    const box = h('div', { class: 'modal' + (opts.wide ? ' wide' : ''), role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'modal-title' }, h('h2', { id: 'modal-title' }, opts.title), opts.desc ? h('p', { class: 'desc' }, opts.desc) : null, opts.body ? h('div', { class: 'body' }, opts.body) : null, opts.actions ? h('div', { class: 'acts' }, opts.actions) : null);
    overlay = h('div', { class: 'modal-overlay', onMousedown: (e) => { if (e.target === overlay && !opts.sticky) close(); } }, box);
    $('#modal-root').appendChild(overlay);
    document.addEventListener('keydown', onKey);
    const first = box.querySelector(opts.focus || 'input,textarea,select,button');
    setTimeout(() => (first || box).focus(), 0);
    return box;
  }
  function confirm(opts) {
    const btn = h('button', { class: 'btn ' + (opts.danger ? 'danger' : 'primary'), type: 'button', onClick: async () => { btn.disabled = true; try { await opts.onConfirm(); close(); } catch (e) { btn.disabled = false; toast(e.message, 'err'); } } }, opts.confirm || t('common.confirm'));
    open({ title: opts.title, desc: opts.desc, actions: [h('button', { class: 'btn secondary', type: 'button', onClick: close }, t('common.cancel')), btn], focus: '.btn.secondary' });
  }
  function info(opts) { open({ title: opts.title, desc: opts.desc, body: opts.body, wide: opts.wide, actions: [h('span'), h('button', { class: 'btn primary', type: 'button', onClick: close }, t('common.close'))] }); }
  return { open, close, confirm, info };
})();

function toast(msg, kind) {
  const host = $('#toasts'); if (!host) return;
  const el = h('div', { class: 'toast' + (kind === 'err' ? ' err' : ''), role: kind === 'err' ? 'alert' : 'status' }, kind === 'learn' ? icon('spark', 't-icon') : kind === 'err' ? icon('alert') : null, h('span', {}, msg));
  host.appendChild(el);
  while (host.children.length > 4) host.firstChild.remove();
  setTimeout(() => { el.style.opacity = '0'; el.style.transition = 'opacity 200ms'; setTimeout(() => el.remove(), 220); }, kind === 'err' ? 7000 : 4200);
}

/* ============================================================ views: settings */

const settings = (() => {
  const PAGES = [['overview', 'home'], ['appearance', 'palette'], ['models', 'cpu'], ['agents', 'users'], ['skills', 'skill'], ['mcp', 'plug'], ['channels', 'channel'], ['automations', 'clock'], ['memory', 'memory'], ['activity', 'pulse'], ['about', 'info']];
  const BUILTIN_TOOLS = ['skill_view', 'skill_manage', 'memory', 'delegate_task', 'web_fetch', 'list_agents', 'list_sessions', 'read_session', 'current_time'];
  let viewEl, navEl, bodyEl, page = null, token = 0;

  function view() {
    navEl = h('nav', { class: 'settings-nav', 'aria-label': t('settings.title') });
    bodyEl = h('div', { class: 'settings-body' });
    viewEl = h('section', { id: 'view-settings', class: 'settings', hidden: true }, navEl, bodyEl);
    return viewEl;
  }
  function renderNav() {
    clear(navEl);
    navEl.appendChild(h('a', { class: 'back quiet', href: '#/' + (S.current ? 's/' + S.current.id : '') }, icon('chev-left'), t('settings.backToChat')));
    for (const [p, ic] of PAGES) navEl.appendChild(h('a', { href: '#/settings/' + p, 'aria-current': p === page ? 'page' : null, class: 'quiet' }, icon(ic), h('span', {}, t('settings.' + p))));
  }
  function hide() { if (viewEl) viewEl.hidden = true; }
  function show(p, query) {
    if (!viewEl) return;
    page = PAGES.some((x) => x[0] === p) ? p : 'overview';
    viewEl.hidden = false; const tv = $('#view-thread'); if (tv) tv.hidden = true;
    renderNav(); renderTopbar(); renderThemeBtn();
    const my = ++token;
    clear(bodyEl);
    const grid = h('div', { class: 'settings-grid' + (page === 'agents' || page === 'activity' || page === 'automations' ? ' wide' : '') });
    bodyEl.appendChild(grid); bodyEl.scrollTop = 0;
    const render = pages[page];
    grid.appendChild(h('div', { class: 'status-line' }, icon('spinner', 'spin'), t('common.loading')));
    Promise.resolve().then(() => render(query || {})).then((node) => { if (my !== token) return; clear(grid); append(grid, [node]); })
      .catch((e) => { if (my !== token) return; clear(grid); grid.appendChild(h('div', { class: 'error-banner' }, icon('alert'), h('span', { class: 'grow' }, e.message), h('button', { type: 'button', onClick: () => show(page, query) }, t('common.retry')))); });
  }
  function reload() { show(page, S.route.query); }

  /* ---- building blocks ---- */
  const header = (title, desc, ...actions) => h('div', { class: 'settings-header row', style: 'justify-content:space-between' }, h('div', {}, h('h1', {}, title), desc ? h('p', {}, desc) : null), actions.length ? h('div', { class: 'row' }, actions) : null);
  const section = (title, ...children) => h('div', {}, title ? h('h2', { class: 'settings-section-title' }, title) : null, h('div', { class: 'settings-card' }, children));
  const sectionWith = (title, actions, ...children) => h('div', {}, h('div', { class: 'settings-section-heading' }, h('h2', { class: 'settings-section-title' }, title), h('div', { class: 'row' }, actions)), h('div', { class: 'settings-card' }, children));
  function row(title, desc, control, opts) {
    opts = opts || {};
    return h('div', { class: 'settings-row settings-hover' + (opts.switch ? ' switch-row' : '') + (opts.stack ? ' stack' : '') }, h('div', { class: 'r-label' }, h('div', { class: 'r-title' }, title), desc ? h('div', { class: 'r-desc' }, desc) : null), control ? h('div', { class: 'r-control' }, control) : null);
  }
  function switchEl(checked, onChange, label) {
    const b = h('button', { class: 'switch', type: 'button', role: 'switch', 'aria-checked': String(!!checked), 'aria-label': label || '' });
    b.addEventListener('click', async () => { const next = b.getAttribute('aria-checked') !== 'true'; b.setAttribute('aria-checked', String(next)); if (onChange) { try { await onChange(next); } catch (e) { b.setAttribute('aria-checked', String(!next)); toast(e.message, 'err'); } } });
    b.checked = () => b.getAttribute('aria-checked') === 'true';
    return b;
  }
  const input = (attrs) => h('input', Object.assign({ class: 'field', type: 'text' }, attrs));
  const textarea = (attrs) => h('textarea', Object.assign({ class: 'field' }, attrs));
  function select(options, value, attrs) { const s = h('select', Object.assign({ class: 'field' }, attrs)); for (const o of options) { const [v, label] = Array.isArray(o) ? o : [o, o]; s.appendChild(h('option', { value: v, selected: v === value }, label)); } return s; }
  function multi(options, selected, name) {
    const box = h('div', { class: 'multi', role: 'group', 'aria-label': name });
    const set = new Set(selected || []);
    for (const o of options) { const [v, label] = Array.isArray(o) ? o : [o, o]; box.appendChild(h('label', {}, h('input', { type: 'checkbox', value: v, checked: set.has(v) }), label)); }
    box.values = () => [...box.querySelectorAll('input:checked')].map((i) => i.value);
    return box;
  }
  const field = (label, control, hint, span) => h('div', { class: span ? 'span' : '' }, h('label', { class: 'lbl' }, label), control, hint ? h('div', { class: 'hint', style: 'margin-top:0.3rem' }, hint) : null);
  function saveBtn(label, fn) {
    const b = h('button', { class: 'btn primary', type: 'button' }, label || t('common.save'));
    b.addEventListener('click', async () => { const txt = b.textContent; b.disabled = true; b.textContent = t('common.saving'); try { await fn(); b.textContent = t('common.saved'); setTimeout(() => { b.textContent = txt; b.disabled = false; }, 1200); } catch (e) { b.textContent = txt; b.disabled = false; toast(e.message, 'err'); } });
    return b;
  }
  const statusPill = (status) => h('span', { class: 'pill ' + (status === 'connected' ? 'ok' : status === 'error' || status === 'unauthorized' ? 'err' : status === 'disabled' ? '' : 'warn') }, t('mcp.status.' + status) === 'mcp.status.' + status ? status : t('mcp.status.' + status));
  const empty = (msg) => h('div', { class: 'empty' }, msg);
  const stat = (k, v, s) => h('div', { class: 'stat' }, h('div', { class: 'k' }, k), h('div', { class: 'v', title: String(v) }, v), s ? h('div', { class: 's' }, s) : null);
  const agentName = (id) => { const a = agentById(id); return a ? '@' + a.handle : id ? id.slice(0, 8) : t('memory.shared'); };

  const pages = {};

  /* ---- overview ---- */
  pages.overview = async () => {
    const info = await api.get('/api/info'); S.info = info;
    const u = info.usage24h || {};
    return h('div', { class: 'settings-stack' },
      header(t('settings.overview'), `${info.name} ${info.version}`),
      section(t('overview.status'), h('div', { class: 'stat-grid' },
        stat(t('overview.uptime'), fmtDuration(info.uptimeMs), fmtDate(info.startedAt)),
        stat(t('overview.db'), info.turso && info.turso.pingMs != null ? `${info.turso.pingMs} ms` : '—', info.turso && info.turso.host),
        stat(t('overview.llm'), (info.llm && info.llm.model) || '—', info.llm && `${info.llm.kind}${info.llm.apiKeySet ? '' : ' · ' + t('models.apiKeyNone')}`),
        stat(t('overview.telegram'), info.telegram && info.telegram.enabled ? t('common.enabled') : t('common.disabled')),
        stat(t('overview.agents'), info.agents))),
      section(t('overview.usage'), h('div', { class: 'stat-grid' }, stat(t('overview.requests'), fmtNum(u.requests)), stat(t('overview.promptTokens'), fmtNum(u.promptTokens)), stat(t('overview.completionTokens'), fmtNum(u.completionTokens)))),
      section(null,
        row(t('overview.publicUrl'), t('models.publicUrlDesc'), h('span', { class: 'r-value' }, info.publicUrl || t('overview.notSet'))),
        row(t('overview.auth'), null, h('span', { class: 'r-value' }, info.authRequired ? t('common.yes') : t('common.no'))),
        info.authRequired ? row(t('login.logout'), null, h('button', { class: 'btn outline sm', type: 'button', onClick: async () => { await api.post('/api/auth/logout'); ws.close(); S.auth.authenticated = false; renderRoot(); } }, icon('logout'), t('login.logout'))) : null));
  };

  /* ---- appearance ---- */
  pages.appearance = async () => {
    const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': t('theme.title') });
    const paint = () => seg.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.mode === theme.get())));
    for (const m of ['system', 'light', 'dark']) seg.appendChild(h('button', { type: 'button', role: 'radio', dataset: { mode: m }, onClick: () => { theme.set(m); paint(); renderThemeBtn(); } }, t('theme.' + m)));
    paint();
    return h('div', { class: 'settings-stack' }, header(t('settings.appearance')),
      section(null,
        row(t('theme.title'), t('theme.desc'), seg),
        row(t('appearance.sidebar'), t('appearance.sidebarDesc'), switchEl(!S.sidebarCollapsed, () => { S.sidebarCollapsed = !S.sidebarCollapsed; try { localStorage.setItem('kongming.sidebar', S.sidebarCollapsed ? 'collapsed' : 'open'); } catch (e) { /* ignore */ } syncSidebarClass(); }, t('appearance.sidebar')), { switch: true }),
        row(t('appearance.motion'), t('appearance.motionDesc'), h('span', { class: 'r-value' }, reducedMotion() ? t('common.enabled') : t('common.disabled')))));
  };

  /* ---- models ---- */
  pages.models = async () => {
    const st = await api.get('/api/settings'); const llm = st.llm || {};
    const kind = select([['openai', 'OpenAI-compatible'], ['anthropic', 'Anthropic']], llm.kind || 'openai');
    const baseUrl = input({ value: llm.baseUrl || '', placeholder: 'https://api.openai.com/v1' });
    const apiKey = input({ type: 'password', autocomplete: 'off', placeholder: llm.apiKeySet ? '••••••••' : 'sk-…' });
    const model = input({ value: llm.model || '', placeholder: 'gpt-4o-mini' });
    const maxTokens = input({ type: 'number', min: 1, value: llm.maxTokens == null ? '' : llm.maxTokens, placeholder: 'auto' });
    const temperature = input({ type: 'number', min: 0, max: 2, step: 0.1, value: llm.temperature == null ? '' : llm.temperature, placeholder: 'default' });
    const effort = select([['', t('common.none')], ['low', 'low'], ['medium', 'medium'], ['high', 'high']], llm.effort || '');
    const keyState = h('span', { class: 'pill ' + (llm.apiKeySet ? 'ok' : 'warn') }, llm.apiKeySet ? t('models.apiKeySet') : t('models.apiKeyNone'));
    const testOut = h('div', { class: 'hint', style: 'margin-top:0.4rem' });
    const save = saveBtn(null, async () => {
      const body = { kind: kind.value, baseUrl: baseUrl.value.trim(), model: model.value.trim(), maxTokens: maxTokens.value ? Number(maxTokens.value) : null, temperature: temperature.value !== '' ? Number(temperature.value) : null, effort: effort.value || null };
      if (apiKey.value) body.apiKey = apiKey.value;
      const res = await api.put('/api/settings', { llm: body }); apiKey.value = '';
      keyState.textContent = res.llm.apiKeySet ? t('models.apiKeySet') : t('models.apiKeyNone'); keyState.className = 'pill ' + (res.llm.apiKeySet ? 'ok' : 'warn');
    });
    const test = h('button', { class: 'btn outline', type: 'button', onClick: async () => { test.disabled = true; testOut.textContent = t('models.testing'); try { const r = await api.post('/api/settings/test-llm'); testOut.textContent = t('models.testOk', r); } catch (e) { testOut.textContent = e.message; testOut.classList.add('danger-text'); } test.disabled = false; } }, t('models.test'));
    const publicUrl = input({ value: st.publicUrl || '', placeholder: 'https://kongming.example.com' });
    return h('div', { class: 'settings-stack' }, header(t('settings.models')),
      section(t('overview.llm'),
        row(t('models.provider'), t('models.providerDesc'), kind),
        row(t('models.baseUrl'), t('models.baseUrlDesc'), baseUrl),
        row(t('models.apiKey'), t('models.apiKeyDesc'), h('div', { class: 'row', style: 'flex-wrap:nowrap;width:100%' }, keyState, apiKey, h('button', { class: 'btn ghost sm', type: 'button', title: t('models.clearKey'), onClick: () => modal.confirm({ title: t('models.clearKey'), confirm: t('common.confirm'), danger: true, onConfirm: async () => { await api.put('/api/settings', { llm: { clearApiKey: true } }); reload(); } }) }, icon('x')))),
        row(t('models.model'), null, model),
        row(t('models.maxTokens'), null, maxTokens),
        row(t('models.temperature'), null, temperature),
        row(t('models.effort'), null, effort),
        h('div', { class: 'settings-footer' }, h('div', {}, test, testOut), h('div', { class: 'acts' }, save))),
      section(t('models.reflection'), row(t('models.reflection'), t('models.reflectionDesc'), switchEl(!(st.reflection && st.reflection.enabled === false), (v) => api.put('/api/settings', { reflection: { enabled: v } }), t('models.reflection')), { switch: true })),
      section(t('models.publicUrl'), row(t('models.publicUrl'), t('models.publicUrlDesc'), publicUrl), h('div', { class: 'settings-footer' }, h('span'), h('div', { class: 'acts' }, saveBtn(null, () => api.put('/api/settings', { publicUrl: publicUrl.value.trim() }))))));
  };

  /* ---- agents ---- */
  pages.agents = async (query) => {
    await Promise.all([loadAgents(), S.mcp.length ? null : loadMcp().catch(() => {}), loadSkills().catch(() => {})]);
    const wrap = h('div', { class: 'settings-stack' });
    const editing = query.edit === 'new' ? {} : query.edit ? agentById(query.edit) : null;
    wrap.appendChild(header(t('agents.title'), t('agents.desc'), h('button', { class: 'btn primary', type: 'button', onClick: () => navigate('#/settings/agents?edit=new') }, icon('plus'), t('agents.new'))));
    if (editing) { wrap.appendChild(agentEditor(editing)); return wrap; }
    // Org chart: managers → reports.
    const byMgr = new Map();
    for (const a of S.agents) { const k = a.managerId && agentById(a.managerId) ? a.managerId : null; if (!byMgr.has(k)) byMgr.set(k, []); byMgr.get(k).push(a); }
    const tree = (parent, depth) => h('ul', {}, (byMgr.get(parent) || []).map((a) => h('li', {}, h('button', { class: 'org-node', type: 'button', onClick: () => navigate('#/settings/agents?edit=' + a.id) }, h('span', { class: 'avatar', style: `--h:${hue(a.handle)}`, 'aria-hidden': 'true' }, a.handle.slice(0, 2)), h('span', {}, h('b', { style: 'font-weight:500' }, a.name), ' ', h('span', { class: 'role' }, '@' + a.handle + (a.role ? ' · ' + a.role : '')), a.isDefault ? h('span', { class: 'pill', style: 'margin-left:0.4rem' }, t('common.default')) : null)), depth < 8 && byMgr.get(a.id) ? tree(a.id, depth + 1) : null)));
    wrap.appendChild(section(t('agents.orgChart'), h('div', { class: 'org-tree' }, tree(null, 0))));
    const list = h('div', {});
    for (const a of S.agents) {
      const mgr = a.managerId && agentById(a.managerId);
      list.appendChild(h('div', { class: 'list-row' },
        h('span', { class: 'avatar', style: `--h:${hue(a.handle)};width:2rem;height:2rem;font-size:12px`, 'aria-hidden': 'true' }, a.handle.slice(0, 2)),
        h('div', { class: 'lr-main' }, h('div', { class: 'lr-title' }, a.name, h('span', { class: 'pill mono' }, '@' + a.handle), a.role ? h('span', { class: 'muted small' }, a.role) : null, a.isDefault ? h('span', { class: 'pill ok' }, t('common.default')) : null),
          h('div', { class: 'lr-sub' }, [a.description, mgr ? t('agents.reports', { handle: mgr.handle }) : t('agents.noReports'), a.model ? a.model : null].filter(Boolean).join(' · '))),
        h('div', { class: 'lr-acts' },
          h('button', { class: 'btn ghost sm', type: 'button', onClick: async () => { await newSession(a.id); } }, t('agents.chat')),
          h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.edit'), 'aria-label': t('common.edit'), onClick: () => navigate('#/settings/agents?edit=' + a.id) }, icon('pencil')),
          h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.delete'), 'aria-label': t('common.delete'), disabled: S.agents.length <= 1, onClick: () => deleteAgent(a) }, icon('trash')))));
    }
    wrap.appendChild(section(t('agents.title'), list));
    return wrap;
  };
  function deleteAgent(a) { modal.confirm({ title: t('agents.deleteTitle', { handle: a.handle }), desc: t('agents.deleteDesc'), confirm: t('common.delete'), danger: true, onConfirm: async () => { await api.del('/api/agents/' + a.id); await loadAgents(); navigate('#/settings/agents'); reload(); } }); }
  function agentEditor(a) {
    const isNew = !a.id;
    const handle = input({ value: a.handle || '', placeholder: 'researcher', maxlength: 32 });
    const name = input({ value: a.name || '', placeholder: 'Researcher' });
    const role = input({ value: a.role || '', placeholder: 'Research analyst' });
    const description = textarea({ value: a.description || '', rows: 2, placeholder: 'Finds and summarises sources, checks facts.' });
    const systemPrompt = textarea({ value: a.systemPrompt || '', rows: 8, class: 'field code' });
    const model = input({ value: a.model || '', placeholder: t('common.optional') });
    const manager = select([['', t('common.none')]].concat(S.agents.filter((x) => x.id !== a.id).map((x) => [x.id, `@${x.handle} — ${x.name}`])), a.managerId || '');
    const tools = multi(BUILTIN_TOOLS, a.tools || [], t('agents.tools'));
    const mcpSel = multi(S.mcp.map((m) => [m.id, m.name]), a.mcpServers || [], t('agents.mcp'));
    const pinned = multi(S.skills.map((k) => k.name), a.pinnedSkills || [], t('agents.pinned'));
    const selfImprove = switchEl(a.selfImprove !== false, null, t('agents.selfImprove'));
    const isDefault = switchEl(!!a.isDefault, null, t('agents.isDefault'));
    const body = () => ({ handle: handle.value.trim(), name: name.value.trim(), role: role.value.trim(), description: description.value.trim(), systemPrompt: systemPrompt.value, model: model.value.trim(), managerId: manager.value || null, tools: tools.values(), mcpServers: mcpSel.values(), pinnedSkills: pinned.values(), selfImprove: selfImprove.checked(), isDefault: isDefault.checked() });
    const save = saveBtn(isNew ? t('common.create') : t('common.save'), async () => { if (isNew) await api.post('/api/agents', body()); else await api.put('/api/agents/' + a.id, body()); await loadAgents(); navigate('#/settings/agents'); renderTopbar(); });
    return section(isNew ? t('agents.new') : '@' + a.handle,
      h('div', { class: 'settings-editor' },
        h('div', { class: 'form-grid two' },
          field(t('agents.handle'), handle, t('agents.handleDesc')), field(t('agents.name'), name),
          field(t('agents.role'), role, t('agents.roleDesc')), field(t('agents.manager'), manager),
          field(t('agents.description'), description, t('agents.descriptionDesc'), true),
          field(t('agents.systemPrompt'), systemPrompt, null, true),
          field(t('agents.model'), model, t('agents.modelDesc')), h('div'),
          field(t('agents.tools'), tools, t('agents.toolsDesc'), true),
          field(t('agents.mcp'), S.mcp.length ? mcpSel : h('div', { class: 'hint' }, t('mcp.empty')), t('agents.mcpDesc'), true),
          field(t('agents.pinned'), S.skills.length ? pinned : h('div', { class: 'hint' }, t('skills.empty')), t('agents.pinnedDesc'), true))),
      row(t('agents.selfImprove'), t('agents.selfImproveDesc'), selfImprove, { switch: true }),
      row(t('agents.isDefault'), t('agents.isDefaultDesc'), isDefault, { switch: true }),
      h('div', { class: 'settings-footer' }, isNew ? h('span') : h('button', { class: 'btn danger-ghost sm', type: 'button', disabled: S.agents.length <= 1, onClick: () => deleteAgent(a) }, icon('trash'), t('common.delete')),
        h('div', { class: 'acts' }, h('button', { class: 'btn secondary', type: 'button', onClick: () => navigate('#/settings/agents') }, t('common.cancel')), save)));
  }

  /* ---- skills ---- */
  pages.skills = async (query) => {
    if (query.name) return skillDetail(query.name);
    await loadSkills();
    const wrap = h('div', { class: 'settings-stack' });
    const importTa = textarea({ rows: 8, class: 'field code', placeholder: '---\nname: deploy\ndescription: How to deploy the app\ntags: [ops]\n---\n\n# Steps\n1. …' });
    const importBox = h('details', { class: 'disclosure' }, h('summary', {}, icon('chev', 'chev'), t('skills.import')), h('div', { class: 'mt' }, h('p', { class: 'hint mb' }, t('skills.importDesc')), importTa, h('div', { class: 'row mt', style: 'justify-content:flex-end' }, saveBtn(t('skills.import'), async () => { const k = await api.post('/api/skills/import', { markdown: importTa.value }); navigate('#/settings/skills?name=' + encodeURIComponent(k.name)); }))));
    wrap.appendChild(header(t('skills.title'), t('skills.desc'), h('button', { class: 'btn primary', type: 'button', onClick: () => newSkillModal() }, icon('plus'), t('skills.new'))));
    wrap.appendChild(section(null, h('div', { class: 'settings-editor' }, importBox)));
    const list = h('div', {});
    if (!S.skills.length) list.appendChild(empty(t('skills.empty')));
    for (const k of S.skills) {
      list.appendChild(h('div', { class: 'list-row clickable', role: 'link', tabindex: 0, onClick: (e) => { if (!e.target.closest('button')) navigate('#/settings/skills?name=' + encodeURIComponent(k.name)); }, onKeydown: (e) => { if (e.key === 'Enter') navigate('#/settings/skills?name=' + encodeURIComponent(k.name)); } },
        h('div', { class: 'lr-main' }, h('div', { class: 'lr-title' }, k.name, h('span', { class: 'pill' }, t('skills.version', { version: k.version })), h('span', { class: 'pill' }, t('skills.source', { source: k.source })), (k.tags || []).map((tg) => h('span', { class: 'chip' }, tg))),
          h('div', { class: 'lr-sub' }, `${k.description} · ${t('skills.uses', { count: k.uses || 0 })}`)),
        h('div', { class: 'lr-acts' }, switchEl(k.enabled, (v) => api.put('/api/skills/' + encodeURIComponent(k.name), { enabled: v }), t('common.enabled')), icon('chev'))));
    }
    wrap.appendChild(section(t('skills.title'), list));
    return wrap;
  };
  function newSkillModal() {
    const name = input({ placeholder: 'deploy-app' }); const desc = input({ placeholder: t('skills.description') });
    modal.open({ title: t('skills.new'), body: h('div', { class: 'form-grid' }, field(t('skills.name'), name), field(t('skills.description'), desc)), focus: 'input',
      actions: [h('button', { class: 'btn secondary', type: 'button', onClick: modal.close }, t('common.cancel')), saveBtn(t('common.create'), async () => { const n = name.value.trim(); if (!n) throw new Error(t('skills.name')); await api.put('/api/skills/' + encodeURIComponent(n), { description: desc.value.trim() || n, body: '# ' + n + '\n', tags: [] }); modal.close(); navigate('#/settings/skills?name=' + encodeURIComponent(n)); })] });
  }
  async function skillDetail(name) {
    const k = await api.get('/api/skills/' + encodeURIComponent(name));
    const desc = input({ value: k.description || '' });
    const tags = input({ value: (k.tags || []).join(', ') });
    const body = textarea({ value: k.body || '', class: 'field code', rows: 18 });
    const preview = h('div', { class: 'md sm', html: md.render(k.body || '') });
    body.addEventListener('input', debounce(() => { preview.innerHTML = md.render(body.value); }, 250));
    const mode = h('div', { class: 'seg' }); const editPane = h('div', {}, body); const prevPane = h('div', { class: 'result-box', style: 'max-height:40rem' }, preview); prevPane.hidden = true;
    const setMode = (m) => { editPane.hidden = m !== 'edit'; prevPane.hidden = m !== 'preview'; mode.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.m === m))); };
    mode.append(h('button', { type: 'button', dataset: { m: 'edit' }, onClick: () => setMode('edit') }, t('skills.edit')), h('button', { type: 'button', dataset: { m: 'preview' }, onClick: () => setMode('preview') }, t('skills.preview')));
    setMode('edit');
    const versions = (k.versions || []).slice().sort((a, b) => (b.version || 0) - (a.version || 0));
    const hist = h('div', { class: 'version-list' }, versions.length ? versions.map((v) => h('button', { type: 'button', title: t('skills.restore'), onClick: () => { body.value = v.body || ''; preview.innerHTML = md.render(body.value); if (v.description) desc.value = v.description; setMode('edit'); toast(t('skills.version', { version: v.version })); } }, h('b', { style: 'font-weight:500' }, t('skills.version', { version: v.version })), ' · ', h('span', { class: 'muted' }, fmtDate(v.createdAt)), v.description ? h('div', { class: 'hint' }, v.description) : null)) : empty(t('activity.empty')));
    return h('div', { class: 'settings-stack' },
      header(k.name, `${t('skills.version', { version: k.version })} · ${t('skills.source', { source: k.source })} · ${t('skills.uses', { count: k.uses || 0 })}`,
        h('button', { class: 'btn outline sm', type: 'button', onClick: () => navigate('#/settings/skills') }, icon('chev-left'), t('common.back')),
        h('button', { class: 'btn outline sm', type: 'button', onClick: () => copyText(k.markdown || '').then(() => toast(t('common.copied'))) }, icon('copy'), t('skills.export'))),
      section(t('skills.edit'),
        h('div', { class: 'settings-editor' }, h('div', { class: 'form-grid two' }, field(t('skills.description'), desc), field(t('skills.tags'), tags, t('skills.tagsDesc'))),
          h('div', { class: 'row', style: 'justify-content:space-between' }, h('label', { class: 'lbl', style: 'margin:0' }, t('skills.body')), mode), editPane, prevPane),
        row(t('common.enabled'), null, switchEl(k.enabled, (v) => api.put('/api/skills/' + encodeURIComponent(k.name), { enabled: v }), t('common.enabled')), { switch: true }),
        h('div', { class: 'settings-footer' }, h('button', { class: 'btn danger-ghost sm', type: 'button', onClick: () => modal.confirm({ title: t('skills.deleteTitle', { name: k.name }), desc: t('skills.deleteDesc'), confirm: t('common.delete'), danger: true, onConfirm: async () => { await api.del('/api/skills/' + encodeURIComponent(k.name)); navigate('#/settings/skills'); } }) }, icon('trash'), t('common.delete')),
          h('div', { class: 'acts' }, saveBtn(null, async () => { await api.put('/api/skills/' + encodeURIComponent(k.name), { description: desc.value.trim(), body: body.value, tags: tags.value.split(',').map((x) => x.trim()).filter(Boolean) }); reload(); })))),
      section(t('skills.history'), h('div', { class: 'settings-editor' }, hist)));
  }

  /* ---- mcp ---- */
  /** Sign-in for servers the auth provider cannot redirect back to (kongming
   *  behind a firewall): open the provider in a new tab, then paste the URL
   *  the browser lands on (a localhost page that usually fails to load). */
  function oauthPasteModal(m, r) {
    const pasted = textarea({ rows: 3, class: 'field code', placeholder: r.redirectUri + '?code=…&state=…', spellcheck: 'false' });
    const body = h('div', { class: 'form-grid' },
      h('div', {}, h('b', {}, t('mcp.oauthStep1')), h('div', { class: 'row mt' },
        h('a', { class: 'btn primary sm', href: r.url, target: '_blank', rel: 'noopener noreferrer' }, icon('link'), t('mcp.oauthOpen')),
        h('button', { class: 'btn outline sm', type: 'button', onClick: () => copyText(r.url).then(() => toast(t('common.copied'))) }, icon('copy'), t('mcp.oauthCopyLink')))),
      field(t('mcp.oauthStep2'), pasted, t('mcp.oauthStep2Desc', { uri: r.redirectUri })));
    modal.open({ title: t('mcp.oauthTitle', { name: m.name }), desc: t('mcp.oauthManualDesc'), body, wide: true, sticky: true, focus: 'a',
      actions: [h('button', { class: 'btn secondary', type: 'button', onClick: modal.close }, t('common.cancel')),
        saveBtn(t('mcp.oauthComplete'), async () => {
          const v = pasted.value.trim(); if (!v) throw new Error(t('mcp.oauthStep2'));
          await api.post('/api/mcp/' + m.id + '/oauth/complete', { url: v });
          modal.close(); toast(t('mcp.oauthOk')); reload();
        })] });
  }

  pages.mcp = async (query) => {
    await loadMcp();
    if (query.oauth === 'ok') { toast(t('mcp.oauthOk')); history.replaceState(null, '', '#/settings/mcp'); }
    else if (query.oauth === 'error') { toast(t('mcp.oauthErr', { message: query.message || '' }), 'err'); history.replaceState(null, '', '#/settings/mcp'); }
    const wrap = h('div', { class: 'settings-stack' });
    const editing = query.edit === 'new' ? {} : query.edit ? S.mcp.find((m) => m.id === query.edit) : null;
    wrap.appendChild(header(t('mcp.title'), t('mcp.desc'), h('button', { class: 'btn primary', type: 'button', onClick: () => navigate('#/settings/mcp?edit=new') }, icon('plus'), t('mcp.add'))));
    if (editing) { wrap.appendChild(mcpEditor(editing)); return wrap; }
    if (!S.mcp.length) { wrap.appendChild(section(null, empty(t('mcp.empty')))); return wrap; }
    for (const m of S.mcp) wrap.appendChild(mcpCard(m));
    return wrap;
  };
  function mcpCard(m) {
    const tools = Array.isArray(m.tools) ? m.tools : [];
    const toolSel = select(tools.map((tl) => tl.name), tools[0] && tools[0].name);
    const argsTa = textarea({ rows: 3, class: 'field code', value: '{}' });
    const out = h('div', { class: 'result-box', hidden: true });
    const callBtn = h('button', { class: 'btn outline sm', type: 'button', onClick: async () => {
      let args; try { args = JSON.parse(argsTa.value || '{}'); } catch (e) { toast('Invalid JSON', 'err'); return; }
      callBtn.disabled = true; out.hidden = false; out.className = 'result-box'; out.textContent = t('common.loading');
      try { const r = await api.post('/api/mcp/' + m.id + '/call', { tool: toolSel.value, args }); out.textContent = r.content || ''; if (r.isError) out.classList.add('err'); } catch (e) { out.textContent = e.message; out.classList.add('err'); }
      callBtn.disabled = false;
    } }, t('mcp.call'));
    const act = (label, fn, cls) => { const b = h('button', { class: 'btn ' + (cls || 'outline') + ' sm', type: 'button' }, label); b.addEventListener('click', async () => { b.disabled = true; try { await fn(); } catch (e) { toast(e.message, 'err'); } b.disabled = false; }); return b; };
    const actions = [
      act([icon('refresh'), t('mcp.refresh')], async () => { const r = await api.post('/api/mcp/' + m.id + '/refresh'); if (r.error) toast(r.error, 'err'); reload(); }),
      m.authType === 'oauth' ? act([icon('link'), m.oauthConnected ? t('mcp.disconnect') : t('mcp.connect')], async () => { if (m.oauthConnected) { await api.post('/api/mcp/' + m.id + '/oauth/disconnect'); reload(); } else { const r = await api.post('/api/mcp/' + m.id + '/oauth/start'); if (r.manual) oauthPasteModal(m, r); else location.href = r.url; } }) : null,
      h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.edit'), 'aria-label': t('common.edit'), onClick: () => navigate('#/settings/mcp?edit=' + m.id) }, icon('pencil')),
      h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.delete'), 'aria-label': t('common.delete'), onClick: () => modal.confirm({ title: t('mcp.deleteTitle', { name: m.name }), desc: t('mcp.deleteDesc'), confirm: t('common.delete'), danger: true, onConfirm: async () => { await api.del('/api/mcp/' + m.id); reload(); } }) }, icon('trash')),
    ];
    return section(null,
      h('div', { class: 'list-row' }, h('div', { class: 'lr-main' },
        h('div', { class: 'lr-title' }, m.name, statusPill(m.enabled ? m.status : 'disabled'), h('span', { class: 'pill' }, t('mcp.tools', { count: m.toolCount || 0 })), h('span', { class: 'pill' }, m.transport), h('span', { class: 'pill' }, m.authType + (m.authType === 'oauth' ? (m.oauthConnected ? ' ✓' : '') : m.authType === 'token' && m.hasToken ? ' ✓' : '')),
        h('div', { class: 'lr-sub mono' }, m.url), m.lastError ? h('div', { class: 'lr-sub danger-text' }, m.lastError) : null),
        h('div', { class: 'lr-acts' }, actions)),
      tools.length ? h('div', { class: 'settings-editor', style: 'padding-top:0' },
        h('details', { class: 'disclosure' }, h('summary', {}, icon('chev', 'chev'), t('mcp.showTools'), ` (${tools.length})`), h('div', { class: 'tool-list' }, tools.map((tl) => h('div', { class: 'tl' }, h('b', {}, tl.name), tl.description ? h('p', {}, tl.description) : null)))),
        h('details', { class: 'disclosure' }, h('summary', {}, icon('chev', 'chev'), t('mcp.tryTool')), h('div', { class: 'form-grid two mt' }, field(t('mcp.toolName'), toolSel), field(t('mcp.args'), argsTa)), h('div', { class: 'row mt', style: 'justify-content:flex-end' }, callBtn), h('div', { class: 'mt' }, out)))) : null);
  }
  function mcpEditor(m) {
    const isNew = !m.id;
    const name = input({ value: m.name || '', placeholder: 'github' });
    const url = input({ value: m.url || '', type: 'url', placeholder: 'https://mcp.example.com/mcp' });
    const transport = select([['http', 'Streamable HTTP'], ['sse', 'HTTP + SSE (legacy)']], m.transport || 'http');
    const authType = select([['none', t('common.none')], ['token', 'Bearer token'], ['oauth', 'OAuth 2.1']], m.authType || 'none');
    const token = input({ type: 'password', autocomplete: 'off', placeholder: m.hasToken ? '••••••••' : '' });
    const clearTok = h('input', { type: 'checkbox' });
    const cid = input({ value: m.oauthClientId || '' }); const csec = input({ type: 'password', autocomplete: 'off' }); const scope = input({ value: m.oauthScope || '' });
    const headers = h('div', { class: 'kv-rows' });
    const addHeader = (k, v) => { const r = h('div', { class: 'kv' }, input({ value: k || '', placeholder: 'X-Header', class: 'field sm' }), input({ value: v || '', placeholder: 'value', class: 'field sm' }), h('button', { class: 'icon-btn quiet', type: 'button', 'aria-label': t('common.delete'), onClick: () => r.remove() }, icon('x'))); headers.appendChild(r); };
    for (const k of Object.keys(m.headers || {})) addHeader(k, m.headers[k]);
    const enabled = switchEl(m.enabled !== false, null, t('common.enabled'));
    const tokenRow = h('div', { class: 'span' }, field(t('mcp.token'), token, t('mcp.tokenDesc')), m.hasToken ? h('label', { class: 'hint row', style: 'margin-top:0.4rem' }, clearTok, t('mcp.clearToken')) : null);
    const oauthRows = h('div', { class: 'form-grid two span' }, field(t('mcp.oauthClientId'), cid, t('mcp.oauthDesc')), field(t('mcp.oauthClientSecret'), csec), field(t('mcp.oauthScope'), scope));
    const syncAuth = () => { tokenRow.hidden = authType.value !== 'token'; oauthRows.hidden = authType.value !== 'oauth'; }; authType.addEventListener('change', syncAuth); syncAuth();
    const body = () => {
      const hdr = {}; headers.querySelectorAll('.kv').forEach((r) => { const [k, v] = r.querySelectorAll('input'); if (k.value.trim()) hdr[k.value.trim()] = v.value; });
      const b = { name: name.value.trim(), url: url.value.trim(), transport: transport.value, authType: authType.value, headers: hdr, enabled: enabled.checked(), oauthClientId: cid.value.trim() || null, oauthScope: scope.value.trim() || null };
      if (token.value) b.token = token.value; if (clearTok.checked) b.clearToken = true; if (csec.value) b.oauthClientSecret = csec.value;
      return b;
    };
    return section(isNew ? t('mcp.add') : m.name,
      h('div', { class: 'settings-editor' }, h('div', { class: 'form-grid two' },
        field(t('mcp.name'), name), field(t('mcp.url'), url), field(t('mcp.transport'), transport), field(t('mcp.authType'), authType), tokenRow, oauthRows,
        h('div', { class: 'span' }, h('div', { class: 'row', style: 'justify-content:space-between;margin-bottom:0.3rem' }, h('label', { class: 'lbl', style: 'margin:0' }, t('mcp.headers')), h('button', { class: 'btn ghost xs', type: 'button', onClick: () => addHeader() }, icon('plus'), t('common.add'))), headers))),
      row(t('common.enabled'), null, enabled, { switch: true }),
      h('div', { class: 'settings-footer' }, h('span'), h('div', { class: 'acts' }, h('button', { class: 'btn secondary', type: 'button', onClick: () => navigate('#/settings/mcp') }, t('common.cancel')),
        saveBtn(isNew ? t('common.create') : t('common.save'), async () => { if (isNew) await api.post('/api/mcp', body()); else await api.put('/api/mcp/' + m.id, body()); navigate('#/settings/mcp'); }))));
  }

  /* ---- channels ---- */
  pages.channels = async () => {
    const st = await api.get('/api/settings'); const tg = st.telegram || {};
    const token = input({ type: 'password', autocomplete: 'off', placeholder: tg.tokenSet ? '••••••••' : '123456:ABC…' });
    const allowed = input({ value: (tg.allowedUsers || []).join(', '), placeholder: '12345678, 87654321', inputmode: 'numeric' });
    const state = h('span', { class: 'pill ' + (tg.tokenSet ? 'ok' : 'warn') }, tg.tokenSet ? t('channels.tokenSet') : t('channels.tokenNone'));
    const conn = tg.tokenSet && tg.enabled !== false ? tg.status : null;
    const connPill = !tg.tokenSet || tg.enabled === false ? h('span', { class: 'pill' }, t('channels.notSetUp')) : !conn ? h('span', { class: 'pill warn' }, t('channels.connecting')) : conn.ok ? h('span', { class: 'pill ok' }, t('channels.connectedAs', { bot: '@' + conn.bot })) : h('span', { class: 'pill err' }, t('channels.notConnected'));
    return h('div', { class: 'settings-stack' }, header(t('channels.title'), t('channels.desc')),
      section(t('channels.telegram'),
        row(t('channels.enabled'), null, switchEl(tg.enabled !== false, (v) => api.put('/api/settings', { telegram: { enabled: v } }).then(reload), t('channels.enabled')), { switch: true }),
        row(t('channels.token'), t('channels.tokenDesc'), h('div', { class: 'row', style: 'flex-wrap:nowrap;width:100%' }, state, token, h('button', { class: 'btn ghost sm', type: 'button', title: t('channels.clearToken'), onClick: () => modal.confirm({ title: t('channels.clearToken'), confirm: t('common.confirm'), danger: true, onConfirm: async () => { await api.put('/api/settings', { telegram: { clearToken: true } }); reload(); } }) }, icon('x')))),
        row(t('channels.connection'), conn ? (conn.ok ? (conn.mode === 'webhook' ? t('channels.modeWebhook', { url: conn.webhookUrl }) : t('channels.modePolling')) : conn.error) : t('channels.connectionDesc'), connPill),
        row(t('channels.allowed'), t('channels.allowedDesc'), allowed),
        h('div', { class: 'settings-footer' }, h('span'), h('div', { class: 'acts' }, saveBtn(null, async () => { const b = { allowedUsers: allowed.value }; if (token.value) b.token = token.value; await api.put('/api/settings', { telegram: b }); reload(); })))));
  };

  /* ---- automations ---- */
  pages.automations = async (query) => {
    const [jobs] = await Promise.all([api.get('/api/cron'), loadAgents()]);
    const wrap = h('div', { class: 'settings-stack' });
    const editing = query.edit === 'new' ? {} : query.edit ? jobs.find((j) => j.id === query.edit) : null;
    wrap.appendChild(header(t('automations.title'), t('automations.desc'), h('button', { class: 'btn primary', type: 'button', onClick: () => navigate('#/settings/automations?edit=new') }, icon('plus'), t('automations.new'))));
    if (editing) { wrap.appendChild(cronEditor(editing)); return wrap; }
    const list = h('div', {});
    if (!jobs.length) list.appendChild(empty(t('automations.empty')));
    for (const j of jobs) {
      const runBtn = h('button', { class: 'btn outline sm', type: 'button', onClick: async () => { runBtn.disabled = true; try { await api.post('/api/cron/' + j.id + '/run'); toast(t('automations.started')); } catch (e) { toast(e.message, 'err'); } runBtn.disabled = false; } }, icon('play'), t('automations.runNow'));
      list.appendChild(h('div', { class: 'list-row' },
        h('div', { class: 'lr-main' }, h('div', { class: 'lr-title' }, j.name, h('span', { class: 'pill mono' }, j.schedule), h('span', { class: 'pill' }, agentName(j.agentId)), !j.enabled ? h('span', { class: 'pill' }, t('common.disabled')) : null, j.lastStatus ? h('span', { class: 'pill ' + (j.lastStatus === 'ok' || j.lastStatus === 'done' ? 'ok' : 'err') }, j.lastStatus) : null),
          h('div', { class: 'lr-sub' }, j.prompt), h('div', { class: 'lr-sub' }, [j.lastRunAt ? t('automations.last', { when: relTime(j.lastRunAt), status: j.lastStatus || '—' }) : null, j.nextRunAt ? t('automations.next', { when: fmtDate(j.nextRunAt) }) : null, j.deliverTo ? 'Telegram ' + j.deliverTo : null].filter(Boolean).join(' · '))),
        h('div', { class: 'lr-acts' }, runBtn,
          switchEl(j.enabled, (v) => api.put('/api/cron/' + j.id, { enabled: v }), t('common.enabled')),
          h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.edit'), 'aria-label': t('common.edit'), onClick: () => navigate('#/settings/automations?edit=' + j.id) }, icon('pencil')),
          h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.delete'), 'aria-label': t('common.delete'), onClick: () => modal.confirm({ title: t('automations.deleteTitle', { name: j.name }), confirm: t('common.delete'), danger: true, onConfirm: async () => { await api.del('/api/cron/' + j.id); reload(); } }) }, icon('trash')))));
    }
    wrap.appendChild(section(null, list));
    return wrap;
  };
  function cronEditor(j) {
    const isNew = !j.id;
    const name = input({ value: j.name || '', placeholder: 'Morning briefing' });
    const agent = select(S.agents.map((a) => [a.id, `@${a.handle} — ${a.name}`]), j.agentId || (defaultAgent() || {}).id);
    const schedule = input({ value: j.schedule || '', placeholder: 'every:30  or  0 8 * * 1-5' });
    const prompt = textarea({ value: j.prompt || '', rows: 5 });
    const deliverTo = input({ value: j.deliverTo || '', placeholder: '123456789' });
    const enabled = switchEl(j.enabled !== false, null, t('common.enabled'));
    const body = () => ({ name: name.value.trim(), agentId: agent.value, schedule: schedule.value.trim(), prompt: prompt.value.trim(), deliverTo: deliverTo.value.trim() || null, enabled: enabled.checked() });
    return section(isNew ? t('automations.new') : j.name,
      h('div', { class: 'settings-editor' }, h('div', { class: 'form-grid two' }, field(t('automations.name'), name), field(t('automations.agent'), agent), field(t('automations.schedule'), schedule, t('automations.scheduleDesc')), field(t('automations.deliverTo'), deliverTo, t('automations.deliverToDesc')), field(t('automations.prompt'), prompt, null, true))),
      row(t('common.enabled'), null, enabled, { switch: true }),
      h('div', { class: 'settings-footer' }, h('span'), h('div', { class: 'acts' }, h('button', { class: 'btn secondary', type: 'button', onClick: () => navigate('#/settings/automations') }, t('common.cancel')),
        saveBtn(isNew ? t('common.create') : t('common.save'), async () => { if (isNew) await api.post('/api/cron', body()); else await api.put('/api/cron/' + j.id, body()); navigate('#/settings/automations'); }))));
  }

  /* ---- memory ---- */
  pages.memory = async () => {
    const [mems] = await Promise.all([api.get('/api/memories?limit=300'), loadAgents()]);
    const kind = select(['fact', 'preference', 'lesson', 'project'], 'fact');
    const content = textarea({ rows: 3 }); const keywords = input({ placeholder: 'comma, separated' }); const importance = input({ type: 'number', min: 1, max: 5, value: 2 });
    const scope = select([['', t('memory.shared')]].concat(S.agents.map((a) => [a.id, '@' + a.handle])), '');
    const form = h('details', { class: 'disclosure' }, h('summary', {}, icon('chev', 'chev'), t('memory.add')),
      h('div', { class: 'form-grid two mt' }, field(t('memory.content'), content, null, true), field(t('memory.kind'), kind), field(t('memory.scope'), scope), field(t('memory.keywords'), keywords), field(t('memory.importance'), importance)),
      h('div', { class: 'row mt', style: 'justify-content:flex-end' }, saveBtn(t('common.add'), async () => { await api.post('/api/memories', { kind: kind.value, content: content.value.trim(), keywords: keywords.value.split(',').map((x) => x.trim()).filter(Boolean), importance: Number(importance.value) || 2, agentId: scope.value || null }); reload(); })));
    const list = h('div', {});
    if (!mems.length) list.appendChild(empty(t('memory.empty')));
    for (const m of mems) list.appendChild(h('div', { class: 'list-row' },
      h('div', { class: 'lr-main' }, h('div', { class: 'lr-title', style: 'font-weight:400' }, m.content), h('div', { class: 'lr-sub row' }, h('span', { class: 'pill' }, m.kind), h('span', { class: 'pill' }, agentName(m.agentId)), h('span', { class: 'pill' }, '★ ' + m.importance), (m.keywords || []).map((k) => h('span', { class: 'chip' }, k)), h('span', { class: 'muted' }, relTime(m.updatedAt || m.createdAt)))),
      h('div', { class: 'lr-acts' }, h('button', { class: 'icon-btn quiet', type: 'button', title: t('common.delete'), 'aria-label': t('common.delete'), onClick: () => modal.confirm({ title: t('common.delete') + '?', desc: m.content.slice(0, 140), confirm: t('common.delete'), danger: true, onConfirm: async () => { await api.del('/api/memories/' + m.id); reload(); } }) }, icon('trash')))));
    return h('div', { class: 'settings-stack' }, header(t('memory.title'), t('memory.desc')), section(null, h('div', { class: 'settings-editor' }, form)), section(t('memory.title'), list));
  };

  /* ---- activity ---- */
  pages.activity = async () => {
    const [imps, tasks, usage] = await Promise.all([api.get('/api/improvements?limit=100'), api.get('/api/tasks?limit=100'), api.get('/api/usage'), loadAgents()]);
    const usageRow = (label, u) => stat(label, fmtNum((u || {}).requests) + ' ' + t('overview.requests'), `${fmtNum((u || {}).promptTokens)} in · ${fmtNum((u || {}).completionTokens)} out`);
    const kindLabel = (k) => { const s = t('activity.kind.' + k); return s === 'activity.kind.' + k ? k : s; };
    const impList = h('div', { class: 'activity-log' }, imps.length ? imps.map((im) => h('div', { class: 'list-row' }, icon(im.kind.startsWith('skill') ? 'skill' : im.kind === 'memory_saved' ? 'memory' : 'spark'),
      h('div', { class: 'lr-main' }, h('div', { class: 'lr-title' }, kindLabel(im.kind), h('span', { class: 'pill' }, agentName(im.agentId)), h('span', { class: 'muted small' }, relTime(im.createdAt))), h('div', { class: 'lr-sub' }, im.note)),
      im.sessionId ? h('a', { class: 'btn ghost xs', href: '#/s/' + im.sessionId }, t('activity.openSession')) : null)) : empty(t('activity.empty')));
    const taskList = h('div', { class: 'activity-log' }, tasks.length ? tasks.map((tk) => h('div', { class: 'list-row' },
      h('div', { class: 'lr-main' }, h('div', { class: 'lr-title' }, `${agentName(tk.fromAgentId)} → ${agentName(tk.toAgentId)}`, h('span', { class: 'pill ' + (tk.status === 'done' ? 'ok' : tk.status === 'failed' ? 'err' : 'warn') }, tk.status), h('span', { class: 'muted small' }, relTime(tk.createdAt))),
        h('div', { class: 'lr-sub' }, tk.instruction), tk.result ? h('div', { class: 'lr-sub' }, '→ ' + tk.result) : null),
      h('div', { class: 'lr-acts' }, tk.childSessionId ? h('a', { class: 'btn ghost xs', href: '#/s/' + tk.childSessionId }, t('activity.openSession')) : null, h('a', { class: 'btn ghost xs', href: '#/s/' + tk.sessionId }, icon('chev'))))) : empty(t('activity.empty')));
    return h('div', { class: 'settings-stack' }, header(t('activity.title'), t('activity.desc')),
      section(t('activity.usage'), h('div', { class: 'stat-grid' }, usageRow(t('activity.day'), usage.day), usageRow(t('activity.week'), usage.week), usageRow(t('activity.month'), usage.month))),
      section(t('activity.improvements'), impList), section(t('activity.tasks'), taskList));
  };

  /* ---- about ---- */
  pages.about = async () => {
    const info = S.info || await api.get('/api/info').catch(() => null);
    return h('div', { class: 'settings-stack' }, header(t('about.title'), info ? `${info.name} ${info.version}` : 'kongming'),
      section(null, row(t('about.title'), t('about.desc'), null), row('UI', t('about.ui'), null), info ? row(t('overview.version'), null, h('span', { class: 'r-value' }, info.version)) : null));
  };

  return { view, show, hide, reload };
})();

/* ============================================================ boot */

let bootedOnce = false;
async function boot(afterLogin) {
  theme.apply();
  try {
    const st = await api.get('/api/auth/status');
    S.auth = { required: !!st.authRequired, authenticated: !!st.authenticated || !st.authRequired };
  } catch (e) { S.auth = { required: false, authenticated: true }; }
  if (!S.auth.authenticated) { renderRoot(); return; }
  renderRoot();
  try { await loadAgents(); } catch (e) { toast(e.message, 'err'); }
  try { await loadSessions(); } catch (e) { toast(e.message, 'err'); }
  renderThemeBtn();
  ws.connect();
  await onRoute();
  if (!bootedOnce || afterLogin) {
    bootedOnce = true;
    if (S.route.name === 'chat' && !S.route.id) setTimeout(() => composer.focus(), 50);
  }
}

document.addEventListener('keydown', (e) => {
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.shiftKey && (e.key === 'o' || e.key === 'O')) { e.preventDefault(); S.pendingAgentId = null; S.current = null; navigate('#/'); setTimeout(() => composer.focus(), 0); }
  else if (mod && e.key === ',') { e.preventDefault(); navigate('#/settings/overview'); }
  else if (mod && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); toggleSidebar(); }
});
document.addEventListener('visibilitychange', () => { if (!document.hidden && S.auth.authenticated) ws.connect(); });
window.addEventListener('online', () => { if (S.auth.authenticated) ws.connect(); });

boot();
})();
