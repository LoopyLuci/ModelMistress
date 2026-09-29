import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'path'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, 'src'),
      '@/components': path.resolve(__dirname, 'src/components'),
      '@/views': path.resolve(__dirname, 'src/views'),
      '@/store': path.resolve(__dirname, 'src/store'),
      '@/router': path.resolve(__dirname, 'src/router'),
      '@/styles': path.resolve(__dirname, 'src/styles')
    }
  },
  server: {
    port: 5173,
    host: true
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false
  }
})