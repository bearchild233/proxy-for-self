import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  base: '/',
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      '@plugins': fileURLToPath(new URL('../plugins', import.meta.url)),
      '@kit': fileURLToPath(new URL('../packages/ui-kit', import.meta.url)),
      '@sdk': fileURLToPath(new URL('../packages/plugin-sdk', import.meta.url)),
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/dev': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
        rewrite: path => path.replace(/^\/dev/, ''),
      },
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    chunkSizeWarningLimit: 600,
    rolldownOptions: {
      checks: {
        invalidAnnotation: false,
      },
      output: {
        codeSplitting: {
          groups: [
            {
              name: 'echarts',
              test: /node_modules\/echarts/,
            },
            {
              name: 'zrender',
              test: /node_modules\/zrender/,
            },
          ],
        },
      },
    },
  },
})
