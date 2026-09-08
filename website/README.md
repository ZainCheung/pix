# Pix website

The Pix website is a self-contained TanStack Start site deployed as the
`pix-website` Cloudflare Worker. It is intentionally separate from the Pix
Relay Worker and does not use relay bindings.

```sh
npm install
npm run dev
```

Validate a production build with:

```sh
npm run typecheck
npm run build
```

The sitemap's `<lastmod>` values are generated before development and production
builds from the last Git commit date for each page's content source. The
generated map lives at `src/generated/sitemap-lastmod.ts`; do not edit its dates
by hand. Use `npm run generate-sitemap-lastmod` when inspecting or refreshing
the map directly.

Product updates live in `content/updates/`, with one MDX file per update. Each
file supplies its own publication date, release stage (`published` or
`preview`), platform, and display order; the updates hub, detail pages,
`llms.txt`, and sitemap are generated from that source.

The static `public/install.sh` is served at `/install.sh` by the website
deployment. It resolves the latest GitHub Release at install time and falls
back to the GitHub Releases page whenever a platform asset is unavailable. On
macOS it extracts and installs the signed app with `ditto`, uses an existing
`/Applications/Pix.app` in preference to creating a user-level copy, and
refuses to choose when both standard app locations exist; `PIX_APP_PATH` can
select a destination explicitly.
The `/appcast.xml` endpoint is a temporary compatibility bridge for the v0.1.4
Sparkle bootstrap build. It returns HTTP 302 with
`Location: https://github.com/ZainCheung/pix/releases/latest/download/appcast.xml`;
v0.1.5+ builds use that GitHub Releases URL directly. The website does not
fetch, validate, or proxy the appcast body.

## Production deploys

`pix-website` deploys from Cloudflare Workers Builds on `main`. The Worker
root directory is `website/`. That only sets the build working directory; it
does not decide when a build starts. Watch paths are evaluated against the
paths in the Git push.

Required Build watch paths:

- Include: `website/*`
- Exclude: empty

Set them in the Cloudflare dashboard: Worker `pix-website` → Settings →
Builds → Build watch paths. Without the include rule, a docs-only or license
change on `main` still deploys the site.
