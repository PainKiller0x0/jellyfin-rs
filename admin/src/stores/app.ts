import { defineStore } from 'pinia';

const THEME_STORAGE_KEY = 'jellyfin-rs-admin-theme';
const SIDEBAR_STORAGE_KEY = 'jellyfin-rs:sidebar-collapsed';

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
    darkMode: storedDarkMode(),
    mobileSidebarOpen: false,
    initialized: false
  }),
  actions: {
    initializeTheme() {
      applyTheme(this.darkMode);
    },
    restore() {
      if (this.initialized) {
        return;
      }

      this.sidebarCollapsed = window.localStorage.getItem(SIDEBAR_STORAGE_KEY) === 'true';
      this.initialized = true;
    },
    toggleSidebar() {
      this.sidebarCollapsed = !this.sidebarCollapsed;
      window.localStorage.setItem(SIDEBAR_STORAGE_KEY, String(this.sidebarCollapsed));
    },
    openMobileSidebar() {
      this.mobileSidebarOpen = true;
    },
    closeMobileSidebar() {
      this.mobileSidebarOpen = false;
    },
    toggleDarkMode() {
      this.darkMode = !this.darkMode;
      window.localStorage.setItem(THEME_STORAGE_KEY, this.darkMode ? 'dark' : 'light');
      applyTheme(this.darkMode);
    }
  }
});
