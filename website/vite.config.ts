import { defineConfig } from 'vite'

import { cloudflare } from '@cloudflare/vite-plugin'
import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import tailwindcss from '@tailwindcss/vite'
import viteReact from '@vitejs/plugin-react'
import { fumadocsMdx } from 'fumadocs-mdx/vite'

export default defineConfig({
  resolve: { tsconfigPaths: true },
  plugins: [
    ...fumadocsMdx(),
    tailwindcss(),
    cloudflare({ viteEnvironment: { name: 'ssr' } }),
    tanstackStart(),
    viteReact(),
  ],
})
