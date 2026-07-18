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
    'admin-surface': 'border border-slate-200 bg-white shadow-sm',
    'admin-page': 'min-h-full bg-[#f5f7fb] p-4 sm:p-6'
  }
});
