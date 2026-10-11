import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';

const binary = resolve(process.argv[2]);
const forkMode = process.argv[3] === '--cli-fork';
const messageMode = process.argv[3] === '--messages';
const root = mkdtempSync(join(tmpdir(), 'hg-rpc-session-'));
const agent = join(root, 'agent');
mkdirSync(agent);
const source = join(root, 'source.jsonl');
const sessions = join(root, 'sessions');
mkdirSync(sessions);
const stamp = '2026-10-10T00:00:00.000Z';
const records = [
  { type: 'session', version: 3, id: '01a0d11a-4780-7ed0-962e-cf60e634e699', timestamp: stamp, cwd: root },
  { type: 'model_change', id: 'model', parentId: null, timestamp: stamp, provider: 'offline', modelId: 'offline' },
  { type: 'thinking_level_change', id: 'thinking', parentId: 'model', timestamp: stamp, thinkingLevel: 'off' },
  { type: 'message', id: 'user', parentId: 'thinking', timestamp: stamp, message: { role: 'user', content: 'original', timestamp: 0 } },
  { type: 'message', id: 'assistant', parentId: 'user', timestamp: stamp, message: { role: 'assistant', content: [{ type: 'text', text: 'answer' }], api: 'openai-completions', provider: 'offline', model: 'offline', usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: 'stop', timestamp: 0 } },
];
writeFileSync(source, records.map(record => JSON.stringify(record)).join('\n') + '\n');
if (forkMode) writeFileSync(join(sessions, 'source.jsonl'), readFileSync(source));
writeFileSync(join(agent, 'models.json'), JSON.stringify({ providers: { offline: { api: 'openai-completions', baseUrl: 'http://127.0.0.1:1', apiKey: 'fixture', models: [{ id: 'offline', reasoning: false, input: ['text'], contextWindow: 128000, maxTokens: 4096 }] } } }));
const onboarding = join(agent, 'omo-senpi', 'omo-native');
mkdirSync(onboarding, { recursive: true });
writeFileSync(join(onboarding, 'onboarding-completed'), '');
const selection = forkMode ? ['--fork', '01a0d11a', '--session-dir', sessions] : ['--session', source];
const child = Bun.spawn([binary, '--mode', 'rpc', '--offline', '--provider', 'offline', '--model', 'offline', ...selection], { cwd: root, env: { PATH: process.env.PATH, HOME: root, MAHO_CODING_AGENT_DIR: agent, MAHO_MEMORY_HOME: join(root, 'memory'), PI_OFFLINE: '1', SENPI_OFFLINE: '1', PI_TELEMETRY: '0' }, stdin: 'pipe', stdout: 'pipe', stderr: 'pipe' });
const stderr = new Response(child.stderr).text();
const pending = new Map();
let sequence = 0;
const reader = (async () => {
  let buffer = '';
  const decoder = new TextDecoder();
  for await (const bytes of child.stdout) {
    buffer += decoder.decode(bytes, { stream: true });
    let end;
    while ((end = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      if (!line) continue;
      const frame = JSON.parse(line);
      if (frame.type === 'response' && pending.has(frame.id)) pending.get(frame.id)(frame);
    }
  }
})();
function request(type, fields = {}) {
  const id = String(++sequence);
  return new Promise((resolveResponse, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`RPC timeout: ${type}`)); }, 15000);
    pending.set(id, frame => { clearTimeout(timer); pending.delete(id); resolveResponse(frame); });
    child.stdin.write(JSON.stringify({ id, type, ...fields }) + '\n');
    child.stdin.flush();
  });
}
try {
  console.log(JSON.stringify({ binarySha256: createHash('sha256').update(readFileSync(binary)).digest('hex'), pid: child.pid }));
  const tree = await request('get_tree');
  assert.equal(tree.success, true);
  assert(Array.isArray(tree.data.tree));
  console.log(JSON.stringify({ initialTree: tree.data }));
  const nodes = [];
  const visit = node => { nodes.push(node); node.children.forEach(visit); };
  tree.data.tree.forEach(visit);
  assert(nodes.some(node => node.entry.id === 'assistant'));
  assert(nodes.some(node => node.entry.id === tree.data.leafId));
  assert(JSON.stringify(tree.data.tree).includes('original'));
  if (messageMode) {
    for (const content of ['appended string', [{ type: 'text', text: 'appended blocks' }]]) assert.equal((await request('append_user_message', { content })).success, true);
    assert.equal((await request('send_custom_message', { customType: 'acceptance', content: 'custom string', display: true })).success, true);
    const messages = (await request('get_messages')).data.messages;
    assert(messages.some(message => message.role === 'user' && message.content === 'appended string'));
    assert(messages.some(message => message.customType === 'acceptance' && message.content === 'custom string'));
    const bash = await request('bash', { command: 'printf INSTALLED_RPC_BASH_OK', excludeFromContext: true });
    assert.equal(bash.success, true);
    assert.equal(bash.data.exitCode, 0);
    assert.equal(bash.data.output, 'INSTALLED_RPC_BASH_OK');
    for (const type of ['abort_branch_summary', 'check_reload_veto']) assert.equal((await request(type)).success, true);
    const edited = await request('edit_user_message', { entryId: 'user', text: 'installed edited user' });
    assert.equal(edited.data.outcome, 'edited');
    assert(JSON.stringify((await request('get_messages')).data.messages).includes('installed edited user'));
    for (const type of ['edit_user_message', 'edit_assistant_message']) {
      assert.equal((await request(type, { entryId: 'missing', text: 'edit' })).errorCode, 'not_found');
      assert.equal((await request(type, { entryId: 'user', text: 42 })).success, false);
    }
    assert.equal((await request('append_user_message', { content: 42 })).success, false);
    const fast = await request('set_fast_mode', { enabled: true });
    assert.equal(fast.success, false);
    assert(fast.error.includes('ChatGPT Subscription'));
    assert.equal((await request('get_fast_mode')).data.enabled, false);
    console.log(JSON.stringify({ messageCommands: true, stringShape: true, edited: edited.data.outcome, fastRefusal: fast.error }));
  } else if (forkMode) {
    const state = (await request('get_state')).data;
    assert.notEqual(state.sessionFile, join(sessions, 'source.jsonl'));
    assert(existsSync(state.sessionFile));
    const header = JSON.parse(readFileSync(state.sessionFile, 'utf8').split('\n')[0]);
    assert.notEqual(header.id, records[0].id);
    assert.equal(header.parentSession, join(sessions, 'source.jsonl'));
    assert.deepEqual(readFileSync(join(sessions, 'source.jsonl')), readFileSync(source));
    const unknown = Bun.spawn([binary, '--fork', 'absent-id', '--session-dir', sessions, '--offline', '-p', 'hi'], { cwd: root, env: { PATH: process.env.PATH, HOME: root, MAHO_CODING_AGENT_DIR: agent }, stdout: 'pipe', stderr: 'pipe' });
    const [output, error, code] = await Promise.all([new Response(unknown.stdout).text(), new Response(unknown.stderr).text(), unknown.exited]);
    assert.equal(code, 1);
    assert(error.includes("No session found matching 'absent-id'"));
    console.log(JSON.stringify({ cliFork: true, newId: header.id, unknownIdExit: code, unknownPid: unknown.pid, outputEmpty: output.length === 0 }));
    let requests = 0;
    const server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
      const body = await request.json();
      assert(body.messages.some(message => message.role === 'user' && (message.content === 'hi' || JSON.stringify(message.content).includes('hi'))));
      requests++;
      const chunk = (delta, finish_reason = null) => `data: ${JSON.stringify({ id: 'fork-qa', object: 'chat.completion.chunk', created: 0, model: 'offline', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
      return new Response(chunk({ role: 'assistant', content: 'FORK_PRINT_OK' }) + chunk({}, 'stop') + 'data: [DONE]\n\n', { headers: { 'content-type': 'text/event-stream' } });
    } });
    try {
      writeFileSync(join(agent, 'models.json'), JSON.stringify({ providers: { offline: { api: 'openai-completions', baseUrl: `http://127.0.0.1:${server.port}/v1`, apiKey: 'fixture', models: [{ id: 'offline', reasoning: false, input: ['text'], contextWindow: 128000, maxTokens: 4096 }] } } }));
      const printed = Bun.spawn([binary, '--fork', '01a0d11a', '--session-dir', sessions, '--offline', '--provider', 'offline', '--model', 'offline', '-p', 'hi'], { cwd: root, env: { PATH: process.env.PATH, HOME: root, MAHO_CODING_AGENT_DIR: agent, MAHO_MEMORY_HOME: join(root, 'memory'), PI_OFFLINE: '1', SENPI_OFFLINE: '1', PI_TELEMETRY: '0' }, stdout: 'pipe', stderr: 'pipe' });
      const timeout = setTimeout(() => printed.kill(), 30000);
      const [answer, errors, exit] = await Promise.all([new Response(printed.stdout).text(), new Response(printed.stderr).text(), printed.exited]);
      clearTimeout(timeout);
      assert.equal(exit, 0, errors);
      assert(answer.includes('FORK_PRINT_OK'));
      assert.equal(requests, 1);
      assert.deepEqual(readFileSync(join(sessions, 'source.jsonl')), readFileSync(source));
      console.log(JSON.stringify({ printFork: true, providerRequests: requests, exit, pid: printed.pid }));
    } finally { server.stop(true); }
  }
  const original = readFileSync(source);
  assert.equal((await request('fork', { entryId: 'absent' })).success, false);
  if (messageMode) {
    // Editing also rewrites source. Import independent assistant history before
    // testing clone persistence, rather than reimporting the edited source.
    const cloneFixture = join(root, 'clone-fixture.jsonl');
    writeFileSync(cloneFixture, records.map(record => JSON.stringify(record)).join('\n') + '\n');
    assert.deepEqual((await request('import_jsonl', { inputPath: cloneFixture })).data, { cancelled: false });
  }
  assert.deepEqual((await request('clone')).data, { cancelled: false });
  const cloned = (await request('get_state')).data.sessionFile;
  assert.notEqual(cloned, source);
  assert(existsSync(cloned));
  assert.deepEqual(readFileSync(source), original);
  assert.equal((await request('fork', { entryId: 'user', position: 'at' })).success, true);
  assert.deepEqual((await request('import_jsonl', { inputPath: source })).data, { cancelled: false });
  assert.equal((await request('navigate_tree', { targetId: 'absent' })).success, false);
  assert.deepEqual(readFileSync(source), original);
  assert.deepEqual((await request('new_session', { parentSession: source })).data, { cancelled: false });
  console.log('RESULT=PASS');
} finally {
  child.kill();
  await child.exited;
  await reader;
  await stderr;
  rmSync(root, { recursive: true, force: true });
  console.log(JSON.stringify({ cleanup: { pid: child.pid, exited: true, tempRemoved: !existsSync(root) } }));
}
