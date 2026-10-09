import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, loadEnv } from 'vite';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '');
  return {
    plugins: [sveltekit()],
    server: {
      proxy: {
        '/api': {
          // VITE_PROXY_TARGET for docker (http://api:8080), fallback to localhost for local dev
          target: env.VITE_PROXY_TARGET || 'http://localhost:7080',
          changeOrigin: true,
          ws: true // Enable WebSocket proxying for /api/v1/library/stream
        }
      }
    }
  };
});
