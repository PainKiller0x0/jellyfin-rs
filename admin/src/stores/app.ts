import { defineStore } from 'pinia';

const THEME_STORAGE_KEY = 'jellyfin-rs-admin-theme';

function storedDarkMode(): boolean {
  return typeof window !== 'undefined' && window.localStorage.getItem(THEME_STORAGE_KEY) === 'dark';
}

function applyTheme(darkMode: boolean) {
  if (typeof document === 'undefined') return;
  document.documentElement.classList.toggle('dark', darkMode);
  document.documentElement.style.colorScheme = darkMode ? 'dark' : 'light';
}

export const useAppStore = defineStore('app', {
  state: () => ({
    sidebarCollapsed: false,
    darkMode: storedDarkMode()
  }),
  actions: {
    initializeTheme() {
      applyTheme(this.darkMode);
    },
    toggleSidebar() {
      this.sidebarCollapsed = !this.sidebarCollapsed;
    },
    toggleDarkMode() {
      this.darkMode = !this.darkMode;
      window.localStorage.setItem(THEME_STORAGE_KEY, this.darkMode ? 'dark' : 'light');
      applyTheme(this.darkMode);
    }
  }
});
