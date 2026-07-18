import { fileURLToPath, URL } from 'node:url';

import UnoCSS from '@unocss/vite';
import vue from '@vitejs/plugin-vue';
import { defineConfig, loadEnv } from 'vite';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd());
  const apiBase = env.VITE_JELLYFIN_API_BASE || 'http://127.0.0.1:8096';

  return {
    base: mode === 'production' ? '/admin/' : '/',
    plugins: [vue(), UnoCSS()],
    resolve: {
      alias: {
        '@': fileURLToPath(new URL('./src', import.meta.url))
      }
    },
    server: {
      port: 5173,
      proxy: {
        '/api': {
          target: apiBase,
          changeOrigin: true,
          rewrite: path => path.replace(/^\/api/, '')
        }
      }
    },
    preview: {
      port: 4173
    },
    build: {
      target: 'es2022'
    }
  };
});
