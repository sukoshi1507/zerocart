# Third-party code

This project is GPL-3.0-or-later: the protocol and the chip databases are ported from FlashGBX by Lesserkuma
(GPL-3.0). The files listed below under "ZeroTag" are MIT code that stays MIT in its own files.

## ZeroTag (MIT)

These files were copied from ZeroTag (https://github.com/sukoshi1507/zerotag) so that this project does not depend
on it. They keep their `SPDX-License-Identifier: MIT` header and the MIT licence below.

- `crates/zerocart-ui/src/{canvas,font,style,widgets,i18n}.rs` (renderer; `i18n.rs` keeps only the `Lang` enum,
  `widgets.rs` lost the amiibo-only helpers)
- `crates/zerocart-app/src/{input,fb,screenshot}.rs` (keyboard, framebuffer, PNG writer)

MIT License

Copyright (c) 2026 sukoshi1507
Copyright (c) 2026 M5Stack CardputerZero Community (application template)

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

## flashkit-md-dotnet (MIT)

`crates/zerocart-core/src/krikzz.rs` and `megadrive.rs` (the Krikzz FlashKit MD wire protocol, the cartridge address
space, flash identification, header plausibility rules) are written from the code and notes of
https://github.com/jfryman/flashkit-md-dotnet, a .NET port of Krikzz's original client. Its licence:

    MIT License

    Copyright (c) 2024 krikzz (original FlashKit MD client and hardware)
    Copyright (c) 2026 James Fryman (cross-platform port)

    Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
    documentation files (the "Software"), to deal in the Software without restriction, including without limitation
    the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and
    to permit persons to whom the Software is furnished to do so, subject to the following conditions: the above
    copyright notice and this permission notice shall be included in all copies or substantial portions of the
    Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
    LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
    SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
    OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
    DEALINGS IN THE SOFTWARE.

## ZeroSend (MIT)

`crates/zerocart-send` implements the LocalSend v2 protocol in Rust from the open protocol (https://github.com/localsend/protocol)
and from the behaviour and notes of ZeroSend, https://github.com/InstantComet/zerosend (a Python application for the
CardputerZero that does the same). ZeroSend's licence:

    MIT License

    Copyright (c) 2026 InstantComet

    Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
    documentation files (the "Software"), to deal in the Software without restriction, including without limitation
    the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and
    to permit persons to whom the Software is furnished to do so, subject to the following conditions: the above
    copyright notice and this permission notice shall be included in all copies or substantial portions of the
    Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
    LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
    SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
    OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
    DEALINGS IN THE SOFTWARE.

ZeroSend and ZeroCart are independent of the LocalSend project, which does not endorse them.
