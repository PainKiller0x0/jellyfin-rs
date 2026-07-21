import { defineStore } from 'pinia';

const SIDEBAR_STORAGE_KEY = 'jellyfin-rs:sidebar-collapsed';

export const useAppStore = defineStore('app', {
  state: () => ({
    sidebarCollapsed: false,
    mobileSidebarOpen: false,
    initialized: false
  }),
  actions: {
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
    }
  }
});
