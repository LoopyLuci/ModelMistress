# The media canvas in Chat

A pane beside the conversation (the **Canvas** button in the Chat header) shows what a conversation holds that is not text, whatever model made it.
It works on the text of a reply, not on a particular model: any model that writes a markdown picture, a link or a fenced drawing gets it shown.

| Kind | Found as | Shown with |
|---|---|---|
| Image | `![alt](url)`, a link or bare URL ending `.png .jpg .gif .webp .avif .bmp`, `data:image/...`, pictures you attach | zoom (wheel, buttons), drag, download |
| Video | `.mp4 .webm .mov .m4v .ogv` | the browser's player |
| Audio | `.mp3 .wav .ogg .flac .m4a .opus .aac` | the browser's player |
| 3D | `.glb .gltf .obj .stl` | three.js with orbit controls (loaded only when a model is opened) |
| Drawing / page | a fenced ```` ```svg ```` or ```` ```html ```` block, `.svg`, `.html` | an `<img>` / a sandboxed frame with scripts off |
| Document / data | `.pdf`, `.txt .md .csv .json .xml .yaml .log` | the PDF viewer / plain text |
| Anything else | `.zip .ply .fbx .usdz` | a download chip |

Only `http(s)`, `data:` and hub paths (`/v1/images/files/...`, fetched with the owner token) are taken; `javascript:` and protocol-relative links are not.
A file on another site (`http(s)`) is **not** fetched by the page: it shows a link to open in a new tab. The page policy (CSP, in `src/trace/ui.rs` and
`desktop-app/tauri.conf.json`) allows `blob:` and `data:` for pictures, media, fetches and frames, and nothing remote, so a reply cannot make the page
load a tracking address. Checked in a browser against a live hub (2026-11-24): SVG, a 768 px PNG, a 1 s WebM from LTX, a WAV, a GLB (WebGL pixels
drawn) and the remote-file note.
Each reply with media gets chips under it; a click opens it in the canvas, and a new asset opens the canvas by itself once. Code: `frontend/src/chat/media.ts`
(finding assets; tests in `media.test.ts`), `frontend/src/components/MediaCanvas.vue`, `ModelViewer.vue`.
Not done: a chat that generates by itself (a model is not wired to call `/v1/images/generations` or `/v1/videos/generations`; today a link or a tool
result that names the file is what the canvas shows), and a remembered canvas position per chat.
