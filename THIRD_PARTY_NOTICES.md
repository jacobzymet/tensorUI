# Third-party notices

## Local OCR runtime

`src/ui/vendor/ocr` contains official npm distributions of Tesseract.js 6.0.1
and Tesseract.js-core 6.1.2 (Apache-2.0), plus English trained data from
`@tesseract.js-data/eng` 1.0.0. That package declares MIT for its packaging;
the upstream trained-data repository supplies Apache-2.0. Both engine license
files and the upstream data license are retained alongside the assets.
Sources, exact versions, and verified npm SHA-512 integrity values are in
`provenance.json`; asset SHA-256 values are in `sha256.json`.
The `*.wasm.js` builds include their WASM payloads; no runtime CDN fetch is needed.

## GLib security backport

`vendor/glib` is glib 0.18.5, copyright the gtk-rs project developers, MIT.
Its LICENSE and COPYRIGHT are retained. `SECURITY_PATCH.md` records the two-line
upstream backport for RUSTSEC-2024-0429.

## DOMPurify

The vendored HTML sanitizer is [DOMPurify 3.4.15](https://github.com/cure53/DOMPurify/releases/tag/3.4.15),
copyright Cure53 and other contributors, distributed under the
[Apache-2.0 or MPL-2.0 license](https://github.com/cure53/DOMPurify/blob/3.4.15/LICENSE).
The upstream license banner is retained in `src/ui/vendor/purify.min.js`.
Source: the tagged upstream `dist/purify.min.js`, retrieved 2026-09-07.
SHA-256 of the downloaded file: `f263b05369e050fa175d4ecb9c9358eb4253602d510297adfb31df48b2f1c4d5`.

## thinking-orbs

The dotted thought-orb animations in
[`src/ui/orb.js`](src/ui/orb.js) are adapted from
[thinking-orbs](https://github.com/Jakubantalik/thinking-orbs)
([demo](https://orbs.jakubantalik.com)), ported to plain canvas for the
TensorMI Harness chat UI.

```
MIT License

Copyright (c) 2026 Jakub Antalik

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

## highlight.js

Syntax highlighting for fenced code blocks in chat uses
[`src/ui/vendor/highlight.min.js`](src/ui/vendor/highlight.min.js)
([highlight.js](https://highlightjs.org/) v11.11.1).

```
BSD 3-Clause License

Copyright (c) 2006, Ivan Sagalaev.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice, this
  list of conditions and the following disclaimer.

* Redistributions in binary form must reproduce the above copyright notice,
  this list of conditions and the following disclaimer in the documentation
  and/or other materials provided with the distribution.

* Neither the name of the copyright holder nor the names of its
  contributors may be used to endorse or promote products derived from
  this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDERS OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
