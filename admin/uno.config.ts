import { defineConfig, presetIcons, presetUno, transformerDirectives, transformerVariantGroup } from 'unocss';

export default defineConfig({
  presets: [
    presetUno(),
    presetIcons({
      scale: 1.1,
      warn: true
    })
  ],
  transformers: [transformerDirectives(), transformerVariantGroup()],
  theme: {
    colors: {
      brand: {
        DEFAULT: '#0f766e',
        soft: '#ccfbf1'
      },
      ink: '#172033'
    }
  },
  shortcuts: {
    'admin-surface': 'border border-[var(--admin-border)] bg-[var(--admin-surface)] shadow-sm',
    'admin-page': 'min-h-full bg-[var(--admin-bg)] p-4 sm:p-6'
  }
});
