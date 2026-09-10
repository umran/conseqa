# conseqa-viz front end

React + TypeScript + Vite, styled with Tailwind CSS v4 and Cloudflare's
Kumo design system. Consumes the page data `conseqa-viz` produces —
`window.CONSEQA` in the embedded build, `public/conseqa.json` in
development.

```
npm install
npm run data     # regenerate public/conseqa.json from the video-streaming example
npm run dev      # http://localhost:5173
npm run build    # typecheck + single-file bundle → dist/index.html
```

`dist/index.html` is committed: the Rust binaries embed it with
`include_str!`, so rebuild and commit it after changing the front end —
and then `cargo build --release`, which is what actually puts the new
bundle inside `conseqa-viz`, `conseqa-confluence` (its `export_spec`
renders through the same code) and `conseqa-harness`. A running MCP
server keeps the image it started with until it is restarted. See
`CONSEQA_VIZ.md` at the repository root for the views, panels, and
report format.

The system view draws two layers. L0 — the application machine — is
always drawn; L1, the declared runtime realization, is a band beneath it
that the top bar switches on and off. `src/lib/runtime.ts` derives what
that band shows, `src/graph/layoutSystem.ts` places both planes, and
`src/lib/citations.ts` resolves the declarations a verdict names so a
runtime-dependent proof can be followed to the facts it rests on.

`REPORT_FORMAT` in `src/types/report.ts` mirrors
`conseqa::analyzer::report::FORMAT`; move it whenever that moves, or the
app refuses the report the binary produces.

`App` also takes an optional `theme` prop. Without it the app owns the
colour mode — restoring the stored choice, setting `data-mode`, and
offering a toggle — which is what the embedded build needs. With it,
the host owns the mode and the app neither persists it nor shows a
toggle, so an application embedding these views has exactly one
control for it.
