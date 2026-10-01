// Markdown themes for golden cases. Golden fixtures must not depend on a chalk version, so the
// ANSI wrapping chalk 5 emits at level 3 is spelled out here.
const wrap = (open, close) => (text) => `\u001b[${open}m${text}\u001b[${close}m`;

const bold = wrap("1", "22");
const dim = wrap("2", "22");
const italic = wrap("3", "23");
const underline = wrap("4", "24");
const strikethrough = wrap("9", "29");
const cyan = wrap("36", "39");
const blue = wrap("34", "39");
const yellow = wrap("33", "39");
const green = wrap("32", "39");

export const plainMarkdownTheme = {
	heading: (text) => text,
	link: (text) => text,
	linkUrl: (text) => text,
	code: (text) => text,
	codeBlock: (text) => text,
	codeBlockBorder: (text) => text,
	quote: (text) => text,
	quoteBorder: (text) => text,
	hr: (text) => text,
	listBullet: (text) => text,
	bold: (text) => text,
	italic: (text) => text,
	strikethrough: (text) => text,
	underline: (text) => text,
};

export const testMarkdownTheme = {
	heading: (text) => bold(cyan(text)),
	link: (text) => blue(text),
	linkUrl: (text) => dim(text),
	code: (text) => yellow(text),
	codeBlock: (text) => green(text),
	codeBlockBorder: (text) => dim(text),
	quote: (text) => italic(text),
	quoteBorder: (text) => dim(text),
	hr: (text) => dim(text),
	listBullet: (text) => cyan(text),
	bold: (text) => bold(text),
	italic: (text) => italic(text),
	strikethrough: (text) => strikethrough(text),
	underline: (text) => underline(text),
};

export const markdownThemes = { plain: plainMarkdownTheme, chalk: testMarkdownTheme };
