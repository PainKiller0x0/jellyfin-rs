import { defineStore } from 'pinia';

import * as authApi from '@/services/auth';
import { tokenStorage } from '@/services/http';
import type { JellyfinUser } from '@/types/jellyfin';

type AuthState = {
  token: string;
  user: JellyfinUser | null;
  initialized: boolean;
};

const storage = tokenStorage();

export const useAuthStore = defineStore('auth', {
  state: (): AuthState => ({
    token: '',
    user: null,
    initialized: false
  }),
  getters: {
    isAuthenticated: state => Boolean(state.token),
    userName: state => state.user?.Name || '管理员'
  },
  actions: {
    restore() {
      if (this.initialized) {
        return;
      }
      this.token = storage.get() ?? '';
      this.initialized = true;
    },
    async login(username: string, password: string) {
      const result = await authApi.login({ username, password });
      this.token = result.AccessToken;
      this.user = result.User;
      this.initialized = true;
      storage.set(result.AccessToken);
    },
    async fetchMe() {
      if (!this.token) {
        return;
      }
      this.user = await authApi.currentUser(this.token);
    },
    async logout() {
      const token = this.token;
      this.token = '';
      this.user = null;
      this.initialized = true;
      storage.remove();

      if (token) {
        await authApi.logout(token).catch(() => undefined);
      }
    }
  }
});
