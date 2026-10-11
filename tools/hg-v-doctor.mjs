import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const binary = resolve(process.argv[2]);
const root = mkdtempSync(join(tmpdir(), 'hg-doctor-'));
try {
  const child = Bun.spawn([binary, 'doctor'], {
    cwd: root,
    env: { PATH: '/usr/bin:/bin', HOME: root, MAHO_CODING_AGENT_DIR: join(root, 'agent') },
    stdout: 'pipe', stderr: 'pipe', stdin: 'ignore',
  });
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ]);
  assert.equal(code, 0, stderr);
  const packagePasses = stdout.split('\n').filter(line => line.startsWith('PASS native executable:'));
  assert.equal(packagePasses.length, 5, stdout);
  assert.deepEqual(readdirSync(root), [], 'doctor must not write into fresh HOME or cwd');
  console.log(JSON.stringify({ code, pid: child.pid, packagePasses, stdout, stderr }));
  console.log('RESULT=PASS');
} finally {
  rmSync(root, { recursive: true, force: true });
  console.log(JSON.stringify({ cleanup: root, removed: true }));
}
