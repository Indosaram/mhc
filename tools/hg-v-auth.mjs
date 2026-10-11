import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const binary = resolve(process.argv[2]);
const root = mkdtempSync(join(tmpdir(), 'hg-auth-'));
const fixtureKey = 'offline-fixture-not-a-real-secret';
try {
  for (const configured of [true, false]) {
    const env = { HOME: root, PATH: '/usr/bin:/bin', MAHO_CODING_AGENT_DIR: join(root, 'agent'),
      SENPI_CODING_AGENT_DIR: join(root, 'agent'), OMO_CODING_AGENT_DIR: join(root, 'agent') };
    if (configured) env.XAI_API_KEY = fixtureKey;
    const child = Bun.spawn([binary, 'auth', 'check', '--provider', 'xai', '--json', '--no-refresh'],
      { cwd: root, env, stdout: 'pipe', stderr: 'pipe' });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    assert(!stdout.includes(fixtureKey) && !stderr.includes(fixtureKey), 'credential must not be exposed');
    const result = JSON.parse(stdout);
    assert.equal(code, configured ? 0 : 1);
    assert.equal(result.status, configured ? 'ready' : 'not_ready');
    assert.equal(result.provider, 'xai');
    if (!configured) assert.equal(result.reason, 'credentials_not_configured');
    assert(!existsSync(join(root, 'agent')), 'auth check must not create credential storage');
    console.log(JSON.stringify({ configured, code, result, stderr, pid: child.pid }));
  }
  const env = { HOME: root, PATH: '/usr/bin:/bin', MAHO_CODING_AGENT_DIR: join(root, 'agent'),
    SENPI_CODING_AGENT_DIR: join(root, 'agent'), OMO_CODING_AGENT_DIR: join(root, 'agent'), XAI_API_KEY: fixtureKey };
  for (const selection of [['--model', 'grok-4.20-0309-non-reasoning'], ['--provider', 'xai', '--model', 'grok-4.20-0309-non-reasoning']]) {
    const child = Bun.spawn([binary, 'auth', 'check', ...selection, '--json', '--no-refresh'],
      { cwd: root, env, stdout: 'pipe', stderr: 'pipe' });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    assert.equal(code, 0);
    const result = JSON.parse(stdout);
    assert.equal(result.status, 'ready');
    assert.equal(result.provider, 'xai');
    assert(!stdout.includes(fixtureKey) && !stderr.includes(fixtureKey));
    console.log(JSON.stringify({ selection, code, result, pid: child.pid }));
  }
  assert(!existsSync(join(root, 'agent')), 'model-aware check must not create storage');
  mkdirSync(join(root, 'agent'), { mode: 0o700 });
  writeFileSync(join(root, 'agent', 'auth.json'), JSON.stringify({ xai: { type: 'api_key', key: fixtureKey } }), { mode: 0o600 });
  {
    const child = Bun.spawn([binary, 'auth', 'check', '--provider', 'xai', '--json', '--credentials', '--no-refresh'],
      { cwd: root, env, stdout: 'pipe', stderr: 'pipe' });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    const result = JSON.parse(stdout);
    assert.equal(code, 0);
    assert.equal(result.credentials, fixtureKey);
    assert(!stderr.includes(fixtureKey));
    assert(!existsSync(join(root, 'agent', 'models.json')), 'auth check must not create catalog storage');
    console.log(JSON.stringify({ command: 'check --credentials', code, credentialMatched: true, pid: child.pid }));
  }
  for (const selection of [['--model', 'grok-4.20-0309-non-reasoning'], ['--provider', 'xai', '--model', 'grok-4.20-0309-non-reasoning']]) {
    const child = Bun.spawn([binary, 'auth', 'print-api-key', ...selection],
      { cwd: root, env, stdout: 'pipe', stderr: 'pipe' });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    assert.equal(code, 0);
    assert.equal(stdout.trim(), fixtureKey);
    assert(!stderr.includes(fixtureKey));
    console.log(JSON.stringify({ selection, command: 'print-api-key', code, credentialMatched: true, pid: child.pid }));
  }
  for (const fixture of [
    { name: 'malformed', content: '{invalid-json', provider: 'xai', code: 2, status: 'invalid' },
    { name: 'unresolved-key', content: JSON.stringify({ xai: { type: 'api_key', key: '$MISSING_AUTH_CHECK_FIXTURE_KEY_9237' } }), provider: 'xai', code: 1, status: 'not_ready' },
    { name: 'expired-no-refresh', content: JSON.stringify({ 'chatgpt-subscription': { type: 'oauth', access: fixtureKey, refresh: 'fixture-refresh', expires: 0 } }), provider: 'chatgpt-subscription', code: 0, status: 'ready' },
  ]) {
    writeFileSync(join(root, 'agent', 'auth.json'), fixture.content, { mode: 0o600 });
    const isolatedEnv = { ...env };
    delete isolatedEnv.XAI_API_KEY;
    const child = Bun.spawn([binary, 'auth', 'check', '--provider', fixture.provider, '--json', '--no-refresh'],
      { cwd: root, env: isolatedEnv, stdout: 'pipe', stderr: 'pipe' });
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    const result = JSON.parse(stdout);
    assert.equal(code, fixture.code);
    assert.equal(result.status, fixture.status);
    assert(!stdout.includes(fixtureKey) && !stderr.includes(fixtureKey));
    console.log(JSON.stringify({ fixture: fixture.name, code, result, pid: child.pid }));
  }
  console.log('RESULT=PASS');
} finally {
  rmSync(root, { recursive: true, force: true });
  console.log(JSON.stringify({ cleanup: root, removed: !existsSync(root) }));
}
