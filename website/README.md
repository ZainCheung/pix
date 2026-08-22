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

The static `public/install.sh` is served at `/install.sh` by the website
deployment. It resolves the latest GitHub Release at install time and falls
back to the GitHub Releases page whenever a platform asset is unavailable.
