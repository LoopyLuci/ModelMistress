/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'path'

export default defineConfig({
  base: './', // relative, so the same build works under the hub's /ui/ and inside the desktop app
  plugins: [vue()],
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts']
  },
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