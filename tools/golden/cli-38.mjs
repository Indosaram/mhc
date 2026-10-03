import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const root = pinnedSenpiRoot();
const source = join(root, "packages/coding-agent/src");
const load = (name) => import(pathToFileURL(join(source, name)).href);
const destination = resolve(import.meta.dir, "../../crates/maho-cli/tests/golden");
mkdirSync(destination, { recursive: true });
const write = (name, data) => {
    writeFileSync(join(destination, name), JSON.stringify(data, null, 2) + "\n");
    console.log(`wrote ${name}`);
};

const { parseGitUrl } = await load("utils/git.ts");
const gitCases = [
    "git:user/repo", "git:github:user/repo@release", "git:gitlab:group/sub/repo",
    "git:bitbucket:user/repo", "git:gist:abcdef", "https://gist.github.com/abcdef",
    "https://github.com/user/repo/tree/main", "https://github.com/user/repo.git#branch%2Fsub",
    "https://www.github.com/user/repo", "https://bitbucket.org/user/repo/src/main",
    "https://gitlab.com/group/sub/repo/-/tree/main", "https://git.sr.ht/~user/repo",
    "git:git.sr.ht/~user/repo@branch", "https://github.com/user/repo?query=1",
    "https://github.com/user/repo%20name", "git:example.org/team/repo@feature/name",
];
write("git-38.json", gitCases.map((input) => ({ input, result: parseGitUrl(input) })));

const { highlight } = await load("utils/syntax-highlight.ts");
const theme = Object.fromEntries(["keyword", "number", "string", "comment", "title", "built_in", "type", "literal", "meta", "regexp", "variable", "params"].map((scope) => [scope, (text) => `[${scope}:${text}]`]));
const snippets = [["typescript", "const value = 1"], ["python", "# hello\nreturn 42"], ["rust", "// hello\nlet value = 42;"], ["javascript", "const re = /foo+/gi;"], ["bash", "echo 'hello'"], ["go", "package main\nfunc main() {}"]];
write("highlight-38.json", snippets.map(([language, code]) => ({ language, code, result: highlight(code, { language, ignoreIllegals: true, theme }) })));

const tests = readFileSync(join(root, "packages/coding-agent/test/image-processing.test.ts"), "utf8");
const fixture = (name) => {
    const match = tests.match(new RegExp(`const ${name}\\s*=\\s*"([^"]+)"`));
    if (!match) throw new Error(`Missing upstream fixture ${name}`);
    return match[1];
};
const { resizeImageInProcess } = await load("utils/image-resize-core.ts");
const { convertToPng } = await load("utils/image-convert.ts");
const cases = [
    ["TINY_PNG", "image/png", { maxWidth: 100, maxHeight: 100, maxBytes: 1048576 }],
    ["TINY_JPEG", "image/jpeg", { maxWidth: 100, maxHeight: 100, maxBytes: 1048576 }],
    ["MEDIUM_PNG_100x100", "image/png", { maxWidth: 50, maxHeight: 50, maxBytes: 1048576 }],
    ["LARGE_PNG_200x200", "image/png", { maxWidth: 2000, maxHeight: 2000, maxBytes: 1 }],
];
const images = [];
for (const [name, mime, options] of cases) {
    const input = fixture(name);
    const resized = await resizeImageInProcess(Buffer.from(input, "base64"), mime, options);
    const converted = await convertToPng(input, mime);
    images.push({ name, input, mime, options, resized, converted });
}
write("images-38.json", images);
