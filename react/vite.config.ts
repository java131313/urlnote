import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';

// API 目标:开发时代理 /api 与 /i(默认本地 server,可用 API_TARGET 覆盖);
// 生产构建用 VITE_API_BASE 编译进前端(跨域部署时设置)。
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '');
  const target = env.API_TARGET || 'http://localhost:8080';
  return {
    plugins: [react()],
    base: env.VITE_BASE_PATH || '/',
    server: {
      port: 5173,
      proxy: {
        '/api': { target, changeOrigin: true },
        '/i': { target, changeOrigin: true },
      },
    },
  };
});
