# Third-party notices

日本語: Fascon 本体は MIT ライセンスと Apache License 2.0 のデュアルライセンス（`LICENSE-MIT`・`LICENSE-APACHE`、どちらか好きな方）です。配布物のうち次の 3 つは、このリポジトリのソースからはビルドされず、コンパイル済みのバイナリまたはバンドルとして同梱しています。そのうち `vscode-extension/out/extension.js` には第三者のパッケージが含まれており、それぞれの著作権表示とライセンス文を下に載せます。

Fascon itself is dual-licensed under the MIT License or the Apache License 2.0, at your option (`LICENSE-MIT`, `LICENSE-APACHE`). Three parts of the release are not built from the sources in
this repository and are shipped in compiled or bundled form. One of them, `vscode-extension/out/extension.js`, contains
third-party packages; their copyright notices and license texts follow.

## Components shipped in compiled or bundled form

| File | What it is | Where it is shipped |
| --- | --- | --- |
| `fascon-core` (`fascon-core.exe` on Windows) | the Fascon 0.1.0 engine, used only for files that use 0.1-only syntax | the VSIX (`extension/bin/<platform>/`) and the platform archives |
| `libmas_runtime.a` | the 0.1.0 runtime library, one per target under `runtime/<target>/` | the platform archives |
| `vscode-extension/out/extension.js` | the 0.1.0 VS Code extension (diagnostics, explanations, dictionary, C generation), bundled and minified | this repository and the VSIX |

`vscode-extension/out/main.js`, the entry point of the 1.0 extension, is plain source and contains no third-party code.

## Third-party packages in `vscode-extension/out/extension.js`

| Package | License | Copyright |
| --- | --- | --- |
| vscode-languageclient | MIT | Copyright (c) Microsoft Corporation |
| vscode-languageserver-protocol | MIT | Copyright (c) Microsoft Corporation |
| vscode-jsonrpc | MIT | Copyright (c) Microsoft Corporation |
| vscode-languageserver-types | MIT | Copyright (c) Microsoft Corporation |
| minimatch | ISC | Copyright (c) 2011-2023 Isaac Z. Schlueter and Contributors |
| brace-expansion | MIT | Copyright (c) 2013 Julian Gruber |
| balanced-match | MIT | Copyright (c) 2013 Julian Gruber |
| semver | ISC | Copyright (c) Isaac Z. Schlueter and Contributors |

The bundle carries no version numbers or license headers. The packages were identified from their code in the bundle (for
example the `vscode-jsonrpc` pipe names, the `^1.82.0` VS Code requirement of vscode-languageclient, minimatch's `GLOBSTAR`,
the brace-expansion escape markers and semver's `SEMVER_SPEC_VERSION`). The texts below are the license files published on
the npm registry with vscode-languageclient 9.0.1, vscode-languageserver-protocol 3.17.5, vscode-jsonrpc 8.2.0,
vscode-languageserver-types 3.17.5, minimatch 5.1.6, brace-expansion 2.0.1, balanced-match 1.0.2 and semver 7.6.3.

### vscode-languageclient, vscode-languageserver-protocol, vscode-jsonrpc, vscode-languageserver-types

The four packages carry the same license file:

```text
Copyright (c) Microsoft Corporation

All rights reserved.

MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED *AS IS*, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

### minimatch

```text
The ISC License

Copyright (c) 2011-2023 Isaac Z. Schlueter and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR
IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

### brace-expansion

```text
MIT License

Copyright (c) 2013 Julian Gruber <julian@juliangruber.com>

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

### balanced-match

```text
(MIT)

Copyright (c) 2013 Julian Gruber <julian@juliangruber.com>

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

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

### semver

```text
The ISC License

Copyright (c) Isaac Z. Schlueter and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR
IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```
