# Third-party notices — maho-browser

This crate ports and vendors code from the MIT-licensed projects below. Their notices are kept here
as the MIT License requires.

## omowright

`assets/page-snapshot.js` is a copy of `src/page-bundle.js` from https://github.com/code-yeongyu/omowright
at a019fdfbc337bd917d10b8f3d64026b3549e0b2c (identifiers renamed, see the file header), and
`src/{cdp,transport,page,keys,computer}.rs` port parts of its `src/pipe.js`, `src/connection.js`,
`src/core.js`, `src/cua.js` and `src/dialog-policy.js`.

```
MIT License

Copyright (c) 2026 Yeongyu Kim

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## pi-cua-integration

`assets/desktop-daemon.py` is a copy of `python/daemon.py` from
https://github.com/code-yeongyu/pi-cua-integration at 9ba0c9e23f13a7fc834c28c65e8733b40a162163, and
`src/computer.rs` ports parts of its `src/cua/daemon.ts`, `src/cua/client.ts` and `src/tools/*.ts`.
Its NOTICE: the package bridges to Cua (https://github.com/trycua/cua, MIT licensed); Cua is a
registered trademark of TryCua Inc.; the package (and this crate) is not affiliated with or
endorsed by TryCua Inc., Anthropic, or OpenAI.

```
MIT License

Copyright (c) 2026 Yeongyu Kim

(Same terms as above.)
```

## oh-my-pi

`src/launch.rs` ports `packages/coding-agent/src/tools/browser/launch.ts` and the tool surface follows
`packages/coding-agent/src/tools/{browser,computer}.ts` from https://github.com/can1357/oh-my-pi at
f59bbac3d56e8b485fef466388b1cb5be4b93123.

```
MIT License

Copyright (c) 2025 Mario Zechner
Copyright (c) 2025-2026 Can Bölük
Copyright (c) 2026 Stencil Labs, Inc.

(Same terms as above.)
```
