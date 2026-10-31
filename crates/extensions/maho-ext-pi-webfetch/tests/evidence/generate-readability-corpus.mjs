// Oracle for tests/fixtures/pinned-readability-corpus.json.
// Expectations: senpi fe8c564b webfetch content.ts + parse-web-document.ts (LinkeDOM), the DOM model this crate
// keeps (parity.d/39.md). Cross-check: pi-webfetch 8a4743da src/webfetch/content.ts (JSDOM).
// Usage: bun tests/evidence/generate-readability-corpus.mjs [--write]
// Without --write it only compares; with --write it rewrites the fixture from the senpi oracle.
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const here = new URL('.', import.meta.url).pathname;
const fixturePath = join(here, '../fixtures/pinned-readability-corpus.json');
const sources = [
    {
        name: 'senpi-linkedom',
        repo: process.env.SENPI_SRC ?? `${process.env.HOME}/code/senpi`,
        pin: 'fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407',
        dir: 'packages/coding-agent/src/core/extensions/builtin/webfetch/webfetch',
        files: ['content.ts', 'parse-web-document.ts'],
        packages: ['linkedom', 'htmlparser2', '@mozilla/readability', 'turndown'],
    },
    {
        name: 'pi-webfetch-jsdom',
        repo: process.env.PI_WEBFETCH_SRC ?? `${process.env.HOME}/.omo/agent/git/github.com/code-yeongyu/pi-webfetch`,
        pin: '8a4743da6a1f32a6a53f4ddae9484c32c78adcd5',
        dir: 'src/webfetch',
        files: ['content.ts'],
        packages: ['jsdom', '@mozilla/readability', 'turndown'],
    },
];

async function run(source, inputs) {
    const work = mkdtempSync(join(tmpdir(), `oracle-${source.name}-`));
    try {
        for (const file of source.files) {
            const shown = Bun.spawn(['git', 'show', `${source.pin}:${source.dir}/${file}`], { cwd: source.repo, stdout: 'pipe', stderr: 'inherit' });
            const text = await new Response(shown.stdout).text();
            if (await shown.exited !== 0) throw new Error(`${source.name}: git show ${file} failed`);
            writeFileSync(join(work, file), text);
            console.error(`${source.name} ${source.pin} ${source.dir}/${file} sha256=${createHash('sha256').update(text).digest('hex')}`);
        }
        symlinkSync(join(source.repo, 'node_modules'), join(work, 'node_modules'));
        for (const name of source.packages) {
            const version = JSON.parse(readFileSync(join(source.repo, 'node_modules', name, 'package.json'), 'utf8')).version;
            console.error(`${source.name} ${name}@${version}`);
        }
        writeFileSync(join(work, 'oracle.ts'), `import { htmlToMarkdown, htmlToText } from "./content.ts";
const input = JSON.parse(await Bun.stdin.text());
console.log(JSON.stringify(input.map((c) => ({ markdown: htmlToMarkdown(c.html, c.url), text: htmlToText(c.html, c.url) }))));
`);
        const child = Bun.spawn(['bun', 'oracle.ts'], { cwd: work, stdin: new Blob([JSON.stringify(inputs)]), stdout: 'pipe', stderr: 'inherit' });
        const output = await new Response(child.stdout).text();
        const exit = await child.exited;
        if (exit !== 0) throw new Error(`${source.name}: oracle exited ${exit}`);
        console.error(`${source.name} oracle exit=${exit}`);
        return JSON.parse(output);
    } finally {
        rmSync(work, { recursive: true, force: true });
    }
}

const fixture = JSON.parse(readFileSync(fixturePath, 'utf8'));
const inputs = fixture.map(({ name, html, url }) => ({ name, html, url }));
const [senpi, jsdom] = [await run(sources[0], inputs), await run(sources[1], inputs)];
let fixtureMatches = 0;
let jsdomMatches = 0;
inputs.forEach((input, index) => {
    const expected = fixture[index];
    const same = (out) => out.markdown === expected.markdown && out.text === expected.text;
    if (same(senpi[index])) fixtureMatches += 1;
    else console.error(`senpi-linkedom DIFF ${input.name}`);
    if (same(jsdom[index])) jsdomMatches += 1;
    else {
        for (const field of ['markdown', 'text']) {
            if (jsdom[index][field] !== expected[field]) {
                console.error(`pi-webfetch-jsdom DIFF ${input.name} ${field}\n  linkedom: ${JSON.stringify(expected[field])}\n  jsdom:    ${JSON.stringify(jsdom[index][field])}`);
            }
        }
    }
});
console.error(`senpi-linkedom matches fixture ${fixtureMatches}/${inputs.length}`);
console.error(`pi-webfetch-jsdom matches fixture ${jsdomMatches}/${inputs.length}`);
if (process.argv.includes('--write')) {
    writeFileSync(fixturePath, `${JSON.stringify(inputs.map((input, index) => ({ ...input, ...senpi[index] })), null, 2)}\n`);
    console.error('fixture rewritten from senpi-linkedom');
} else if (fixtureMatches !== inputs.length) {
    process.exit(1);
}
