<script setup lang="ts">
import { computed } from 'vue';
import { useRoute, useRouter } from 'vue-router';

import { useAppStore } from '@/stores/app';
import { useAuthStore } from '@/stores/auth';

type MenuItem = {
  path: string;
  title: string;
  icon: string;
};

const route = useRoute();
const router = useRouter();
const appStore = useAppStore();
const authStore = useAuthStore();

const menuItems: MenuItem[] = [
  {
    path: '/dashboard',
    title: '控制台',
    icon: 'DataLine'
  },
  {
    path: '/libraries',
    title: '媒体库',
    icon: 'FolderOpened'
  }
];

const activeMenu = computed(() => route.path);

function handleSelect(path: string) {
  router.push(path);
}

async function handleCommand(command: string) {
  if (command === 'logout') {
    await authStore.logout();
    await router.replace('/login');
  }
}
</script>

<template>
  <ElContainer class="admin-layout">
    <ElAside class="admin-layout__aside" :width="appStore.sidebarCollapsed ? '72px' : '232px'">
      <div class="admin-layout__brand" :class="{ 'is-collapsed': appStore.sidebarCollapsed }">
        <div class="admin-layout__logo">JR</div>
        <div v-if="!appStore.sidebarCollapsed" class="admin-layout__brand-copy">
          <strong>jellyfin-rs</strong>
          <span>Server Admin</span>
        </div>
      </div>

      <ElMenu
        :collapse="appStore.sidebarCollapsed"
        :default-active="activeMenu"
        class="admin-layout__menu"
        @select="handleSelect"
      >
        <ElMenuItem v-for="item in menuItems" :key="item.path" :index="item.path">
          <ElIcon>
            <component :is="item.icon" />
          </ElIcon>
          <template #title>{{ item.title }}</template>
        </ElMenuItem>
      </ElMenu>
    </ElAside>

    <ElContainer>
      <ElHeader class="admin-layout__header">
        <div class="admin-layout__header-left">
          <ElButton circle text @click="appStore.toggleSidebar">
            <ElIcon>
              <Fold v-if="!appStore.sidebarCollapsed" />
              <Expand v-else />
            </ElIcon>
          </ElButton>
          <div>
            <div class="admin-layout__title">{{ route.meta.title ?? '管理后台' }}</div>
            <div class="admin-layout__subtitle">轻量 Jellyfin 兼容媒体服务器</div>
          </div>
        </div>

        <ElSpace :size="12">
          <ElTag effect="plain" type="success">Element Plus</ElTag>
          <ElDropdown trigger="click" @command="handleCommand">
            <button class="admin-layout__account" type="button">
              <ElAvatar :size="32">{{ authStore.userName.slice(0, 1).toUpperCase() }}</ElAvatar>
              <span>{{ authStore.userName }}</span>
              <ElIcon>
                <ArrowDown />
              </ElIcon>
            </button>
            <template #dropdown>
              <ElDropdownMenu>
                <ElDropdownItem command="logout">
                  <ElIcon>
                    <SwitchButton />
                  </ElIcon>
                  退出登录
                </ElDropdownItem>
              </ElDropdownMenu>
            </template>
          </ElDropdown>
        </ElSpace>
      </ElHeader>

      <ElMain class="admin-layout__main">
        <RouterView />
      </ElMain>
    </ElContainer>
  </ElContainer>
</template>

<style scoped lang="scss">
.admin-layout {
  min-height: 100vh;
  background: var(--admin-bg);
}

.admin-layout__aside {
  position: sticky;
  top: 0;
  height: 100vh;
  overflow: hidden;
  border-right: 1px solid var(--admin-border);
  background: #ffffff;
  transition: width 160ms ease;
}

.admin-layout__brand {
  display: flex;
  align-items: center;
  gap: 12px;
  height: 64px;
  padding: 0 18px;
  border-bottom: 1px solid var(--admin-border);

  &.is-collapsed {
    justify-content: center;
    padding: 0;
  }
}

.admin-layout__logo {
  display: grid;
  width: 38px;
  height: 38px;
  flex: 0 0 38px;
  place-items: center;
  border-radius: 8px;
  color: #ffffff;
  background: linear-gradient(135deg, #0f766e, #2563eb);
  font-size: 14px;
  font-weight: 800;
}

.admin-layout__brand-copy {
  display: grid;
  gap: 2px;
  min-width: 0;

  strong {
    overflow: hidden;
    font-size: 15px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.admin-layout__menu {
  height: calc(100vh - 64px);
  border-right: 0;
}

.admin-layout__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 64px;
  border-bottom: 1px solid var(--admin-border);
  background: rgb(255 255 255 / 92%);
  backdrop-filter: blur(12px);
}

.admin-layout__header-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.admin-layout__title {
  font-size: 16px;
  font-weight: 700;
}

.admin-layout__subtitle {
  color: var(--admin-muted);
  font-size: 12px;
}

.admin-layout__main {
  min-height: calc(100vh - 64px);
  padding: 0;
}

.admin-layout__account {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 220px;
  height: 40px;
  padding: 0 6px;
  border: 0;
  border-radius: 8px;
  color: var(--admin-text);
  background: transparent;
  cursor: pointer;

  span {
    overflow: hidden;
    max-width: 120px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &:hover {
    background: #f1f5f9;
  }
}

@media (max-width: 760px) {
  .admin-layout__aside {
    position: fixed;
    z-index: 20;
    transform: translateX(-100%);
  }

  .admin-layout__header {
    padding: 0 12px;
  }

  .admin-layout__subtitle {
    display: none;
  }

  .admin-layout__account span,
  .admin-layout__account .el-icon {
    display: none;
  }
}
</style>
