import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Tauri 只在 devServer 端口上拿前端（tauri.conf.json 的 devUrl），所以端口必须写死；
// 端口被占就让它失败，换端口会让 beforeDevCommand 与 devUrl 对不上。
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],

  // tauri.conf.json 的 beforeDevCommand 在 crate 根执行，所以这里的相对路径都以 ui/ 为起点。
  envPrefix: ['VITE_', 'TAURI_ENV_'],

  build: {
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    // Vite 8 的默认压缩器是 rolldown 自带的那个：写 'esbuild' 会去找一个不再随 Vite
    // 分发的 esbuild 包，构建直接失败。
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    outDir: 'dist',
    emptyOutDir: true,
  },

  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    watch: { ignored: ['**/src-tauri/**', '**/target/**'] },
  },
});
