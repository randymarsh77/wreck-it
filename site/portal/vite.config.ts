import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// https://vite.dev/config/
export default defineConfig(({ mode }) => ({
	plugins: [react()],
	define: {
		'import.meta.env.VITE_API_BASE_URL': JSON.stringify(
			process.env.VITE_API_BASE_URL ?? (mode === 'development' ? '' : 'https://gh.wreckit.app')
		),
	},
	server: {
		proxy: {
			'/api': {
				target: process.env.VITE_WORKER_URL ?? 'http://localhost:8787',
				changeOrigin: true,
			},
		},
	},
}));
