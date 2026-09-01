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
file supplies its own publication date, status, release target, and display
order; the updates hub, detail pages, `llms.txt`, and sitemap are generated from
that source.

The static `public/install.sh` is served at `/install.sh` by the website
deployment. It resolves the latest GitHub Release at install time and falls
back to the GitHub Releases page whenever a platform asset is unavailable.

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
