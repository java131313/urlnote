import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5173,
    proxy: {
      // dev: forward API calls to the local Cloudflare Worker (wrangler dev)
      '/api': 'http://localhost:8787',
    },
  },
})
