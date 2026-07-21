<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';

import { useAppStore } from '@/stores/app';
import { useAuthStore } from '@/stores/auth';

type MenuItem = {
  path: string;
  title: string;
  icon: string;
  description: string;
  keywords: string;
};

const route = useRoute();
const router = useRouter();
const appStore = useAppStore();
const authStore = useAuthStore();
const searchVisible = ref(false);
const searchKeyword = ref('');
const searchInputRef = ref<{ focus: () => void }>();
const isMobile = ref(false);
let mobileMediaQuery: MediaQueryList | undefined;

const menuItems: MenuItem[] = [
  {
    path: '/dashboard',
    title: '控制台',
    icon: 'DataLine',
    description: '服务状态、访问日志与实时播放',
    keywords: '首页 状态 日志 会话 地图 dashboard'
  },
  {
    path: '/libraries',
    title: '媒体库',
    icon: 'FolderOpened',
    description: '管理媒体库、目录和扫描任务',
    keywords: '目录 路径 扫描 电影 剧集 library'
  },
  {
    path: '/metadata',
    title: '元数据',
    icon: 'MagicStick',
    description: '搜索、识别与重置媒体元数据',
    keywords: '刮削 识别 tmdb 豆瓣 metadata'
  },
  {
    path: '/playback-stats',
    title: '观影统计',
    icon: 'DataAnalysis',
    description: '查看用户和内容的观影趋势',
    keywords: '播放 时长 排行 统计 playback'
  },
  {
    path: '/tasks',
    title: '计划任务',
    icon: 'Clock',
    description: '运行或停止服务器计划任务',
    keywords: '扫描 定时 后台 task'
  },
  {
    path: '/users',
    title: '用户',
    icon: 'User',
    description: '创建用户并配置访问权限',
    keywords: '账户 管理员 权限 密码 user'
  },
  {
    path: '/settings',
    title: '设置',
    icon: 'Setting',
    description: '配置服务器、元数据源和密钥',
    keywords: 'tmdb 豆瓣 api key server setting'
  }
];

const activeMenu = computed(() => route.path);
const currentMenu = computed(() => menuItems.find(item => item.path === route.path));
const navigationCollapsed = computed(() => !isMobile.value && appStore.sidebarCollapsed);
const filteredMenuItems = computed(() => {
  const keywords = searchKeyword.value.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!keywords.length) {
    return menuItems;
  }

  return menuItems.filter(item => {
    const searchText = [item.title, item.description, item.keywords].join(' ').toLocaleLowerCase();
    return keywords.every(keyword => searchText.includes(keyword));
  });
});

async function handleSelect(path: string) {
  appStore.closeMobileSidebar();
  searchVisible.value = false;
  searchKeyword.value = '';
  if (route.path !== path) {
    await router.push(path);
  }
}

async function handleCommand(command: string) {
  if (command === 'logout') {
    await authStore.logout();
    await router.replace('/login');
  }
}

function openSearch() {
  searchVisible.value = true;
  nextTick(() => searchInputRef.value?.focus());
}

function openFirstSearchResult() {
  const firstResult = filteredMenuItems.value[0];
  if (firstResult) {
    void handleSelect(firstResult.path);
  }
}

function syncMobileViewport(event: MediaQueryListEvent | MediaQueryList) {
  isMobile.value = event.matches;
  if (!event.matches) {
    appStore.closeMobileSidebar();
  }
}

function handleGlobalKeydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === 'k') {
    event.preventDefault();
    openSearch();
  }
}

watch(
  () => route.fullPath,
  () => appStore.closeMobileSidebar()
);

onMounted(() => {
  appStore.restore();
  mobileMediaQuery = window.matchMedia('(max-width: 760px)');
  syncMobileViewport(mobileMediaQuery);
  mobileMediaQuery.addEventListener('change', syncMobileViewport);
  window.addEventListener('keydown', handleGlobalKeydown);
});

onBeforeUnmount(() => {
  mobileMediaQuery?.removeEventListener('change', syncMobileViewport);
  window.removeEventListener('keydown', handleGlobalKeydown);
});
</script>

<template>
  <ElContainer class="admin-layout">
    <button
      v-if="appStore.mobileSidebarOpen"
      aria-label="关闭导航"
      class="admin-layout__scrim"
      type="button"
      @click="appStore.closeMobileSidebar"
    ></button>

    <ElAside
      class="admin-layout__aside"
      :class="{
        'is-collapsed': navigationCollapsed,
        'is-mobile-open': appStore.mobileSidebarOpen
      }"
      :width="navigationCollapsed ? '76px' : '248px'"
    >
      <div class="admin-layout__brand" :class="{ 'is-collapsed': navigationCollapsed }">
        <div class="admin-layout__logo" aria-hidden="true">
          <span></span>
          <span></span>
          <span></span>
        </div>
        <div v-if="!navigationCollapsed" class="admin-layout__brand-copy">
          <strong>jellyfin-rs</strong>
          <span>媒体服务器管理台</span>
        </div>
        <ElButton
          class="admin-layout__mobile-close"
          circle
          text
          title="关闭导航"
          @click="appStore.closeMobileSidebar"
        >
          <ElIcon><Close /></ElIcon>
        </ElButton>
      </div>

      <ElMenu
        :collapse="navigationCollapsed"
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

      <div class="admin-layout__aside-footer" :class="{ 'is-collapsed': navigationCollapsed }">
        <span class="admin-layout__status-dot"></span>
        <div v-if="!navigationCollapsed">
          <strong>服务管理</strong>
          <span>管理会话已连接</span>
        </div>
      </div>
    </ElAside>

    <ElContainer class="admin-layout__content">
      <ElHeader class="admin-layout__header">
        <div class="admin-layout__header-left">
          <ElButton
            class="admin-layout__desktop-toggle"
            circle
            text
            :title="navigationCollapsed ? '展开导航' : '收起导航'"
            @click="appStore.toggleSidebar"
          >
            <ElIcon>
              <Fold v-if="!navigationCollapsed" />
              <Expand v-else />
            </ElIcon>
          </ElButton>
          <ElButton
            class="admin-layout__mobile-toggle"
            circle
            text
            title="打开导航"
            @click="appStore.openMobileSidebar"
          >
            <ElIcon><Menu /></ElIcon>
          </ElButton>
          <div class="admin-layout__page-context">
            <div class="admin-layout__eyebrow">管理后台 / {{ route.meta.title ?? '页面' }}</div>
            <div class="admin-layout__title">{{ currentMenu?.description ?? 'jellyfin-rs 管理后台' }}</div>
          </div>
        </div>

        <div class="admin-layout__header-actions">
          <button class="admin-layout__search-trigger" type="button" @click="openSearch">
            <ElIcon><Search /></ElIcon>
            <span>搜索功能</span>
            <kbd>Ctrl K</kbd>
          </button>
          <ElDropdown trigger="click" @command="handleCommand">
            <button class="admin-layout__account" type="button">
              <ElAvatar :size="34">{{ authStore.userName.slice(0, 1).toUpperCase() }}</ElAvatar>
              <span class="admin-layout__account-copy">
                <strong>{{ authStore.userName }}</strong>
                <small>管理员</small>
              </span>
              <ElIcon>
                <ArrowDown />
              </ElIcon>
            </button>
            <template #dropdown>
              <ElDropdownMenu>
                <ElDropdownItem disabled>
                  <ElIcon><User /></ElIcon>
                  {{ authStore.userName }}
                </ElDropdownItem>
                <ElDropdownItem command="logout">
                  <ElIcon>
                    <SwitchButton />
                  </ElIcon>
                  退出登录
                </ElDropdownItem>
              </ElDropdownMenu>
            </template>
          </ElDropdown>
        </div>
      </ElHeader>

      <ElMain class="admin-layout__main">
        <RouterView v-slot="{ Component }">
          <Transition mode="out-in" name="admin-page">
            <component :is="Component" />
          </Transition>
        </RouterView>
      </ElMain>
    </ElContainer>

    <ElDialog
      v-model="searchVisible"
      class="admin-layout__search-dialog"
      destroy-on-close
      title="前往功能"
      width="min(600px, calc(100vw - 28px))"
      @closed="searchKeyword = ''"
    >
      <ElInput
        ref="searchInputRef"
        v-model="searchKeyword"
        clearable
        placeholder="搜索页面或功能"
        size="large"
        @keyup.enter="openFirstSearchResult"
      >
        <template #prefix><ElIcon><Search /></ElIcon></template>
      </ElInput>

      <div class="admin-layout__search-results">
        <button
          v-for="item in filteredMenuItems"
          :key="item.path"
          class="admin-layout__search-result"
          type="button"
          @click="handleSelect(item.path)"
        >
          <span class="admin-layout__search-icon"><ElIcon><component :is="item.icon" /></ElIcon></span>
          <span>
            <strong>{{ item.title }}</strong>
            <small>{{ item.description }}</small>
          </span>
          <ElIcon><ArrowRight /></ElIcon>
        </button>
        <ElEmpty v-if="!filteredMenuItems.length" :image-size="72" description="没有匹配的功能" />
      </div>
      <template #footer>
        <div class="admin-layout__search-help">输入关键词筛选，点击结果即可跳转</div>
      </template>
    </ElDialog>
  </ElContainer>
</template>

<style scoped lang="scss">
.admin-layout {
  min-height: 100vh;
  background: var(--admin-bg);
}

.admin-layout__aside {
  position: sticky;
  z-index: 30;
  top: 0;
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
  border-right: 1px solid var(--admin-border);
  background: #ffffff;
  box-shadow: 8px 0 30px rgb(15 23 42 / 3%);
  transition:
    width 180ms ease,
    transform 180ms ease;
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
  display: flex;
  align-items: flex-end;
  justify-content: center;
  gap: 3px;
  width: 38px;
  height: 38px;
  flex: 0 0 38px;
  border-radius: 8px;
  background: #0f766e;
  box-shadow: 0 7px 18px rgb(15 118 110 / 22%);

  span {
    width: 4px;
    margin-bottom: 10px;
    border-radius: 2px;
    background: #ffffff;
  }

  span:nth-child(1) {
    height: 9px;
  }

  span:nth-child(2) {
    height: 18px;
  }

  span:nth-child(3) {
    height: 13px;
  }
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
  flex: 1;
  padding: 12px 10px;
  overflow-x: hidden;
  overflow-y: auto;
  border-right: 0;

  :deep(.el-menu-item) {
    height: 44px;
    margin: 3px 0;
    border-radius: 7px;
    color: #526076;
  }

  :deep(.el-menu-item:hover) {
    color: #0f766e;
    background: #f0f7f6;
  }

  :deep(.el-menu-item.is-active) {
    color: #0f766e;
    background: #e8f4f2;
    font-weight: 700;
  }
}

.admin-layout__aside-footer {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 64px;
  margin: 10px;
  padding: 10px 12px;
  border: 1px solid #dce7e5;
  border-radius: 8px;
  background: #f6faf9;

  &.is-collapsed {
    justify-content: center;
    padding: 10px;
  }

  div {
    display: grid;
    min-width: 0;
    gap: 2px;
  }

  strong {
    font-size: 12px;
  }

  span:not(.admin-layout__status-dot) {
    color: var(--admin-muted);
    font-size: 11px;
  }
}

.admin-layout__status-dot {
  width: 8px;
  height: 8px;
  flex: 0 0 8px;
  border-radius: 50%;
  background: #16a34a;
  box-shadow: 0 0 0 4px rgb(22 163 74 / 10%);
}

.admin-layout__content {
  min-width: 0;
}

.admin-layout__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 64px;
  padding: 0 24px;
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

.admin-layout__page-context {
  display: grid;
  min-width: 0;
  gap: 3px;
}

.admin-layout__eyebrow {
  color: #172033;
  font-size: 14px;
  font-weight: 700;
}

.admin-layout__title {
  overflow: hidden;
  color: var(--admin-muted);
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.admin-layout__main {
  min-height: calc(100vh - 64px);
  padding: 0;
}

.admin-layout__account {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 240px;
  min-height: 44px;
  padding: 4px 7px;
  border: 1px solid transparent;
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

.admin-layout__account-copy {
  display: grid;
  min-width: 0;
  text-align: left;

  strong,
  small {
    overflow: hidden;
    max-width: 110px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    font-size: 13px;
  }

  small {
    color: var(--admin-muted);
    font-size: 11px;
  }
}

.admin-layout__header-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.admin-layout__search-trigger {
  display: grid;
  grid-template-columns: auto minmax(80px, 1fr) auto;
  gap: 8px;
  align-items: center;
  width: 220px;
  height: 38px;
  padding: 0 8px 0 11px;
  border: 1px solid var(--admin-border);
  border-radius: 7px;
  color: var(--admin-muted);
  background: #f8fafc;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;

  &:hover {
    border-color: #9ebbb6;
    background: #ffffff;
  }

  kbd {
    padding: 3px 6px;
    border: 1px solid #dce2e9;
    border-bottom-width: 2px;
    border-radius: 5px;
    color: #64748b;
    background: #ffffff;
    font-family: inherit;
    font-size: 11px;
  }
}

.admin-layout__mobile-toggle,
.admin-layout__mobile-close,
.admin-layout__scrim {
  display: none;
}

.admin-layout__search-results {
  display: grid;
  gap: 5px;
  max-height: min(440px, 58vh);
  margin-top: 14px;
  overflow-y: auto;
}

.admin-layout__search-result {
  display: grid;
  grid-template-columns: 40px minmax(0, 1fr) auto;
  gap: 12px;
  align-items: center;
  width: 100%;
  min-height: 60px;
  padding: 8px 12px;
  border: 1px solid transparent;
  border-radius: 7px;
  color: var(--admin-text);
  background: transparent;
  font: inherit;
  text-align: left;
  cursor: pointer;

  &:hover,
  &:focus-visible {
    border-color: #cfe0dd;
    outline: 0;
    background: #f2f8f7;
  }

  > span:nth-child(2) {
    display: grid;
    min-width: 0;
    gap: 3px;
  }

  strong {
    font-size: 14px;
  }

  small {
    overflow: hidden;
    color: var(--admin-muted);
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  > .el-icon {
    color: #94a3b8;
  }
}

.admin-layout__search-icon {
  display: grid;
  width: 40px;
  height: 40px;
  place-items: center;
  border-radius: 7px;
  color: #0f766e;
  background: #e6f4f1;
  font-size: 18px;
}

.admin-layout__search-help {
  color: var(--admin-muted);
  font-size: 12px;
  text-align: left;
}

.admin-page-enter-active,
.admin-page-leave-active {
  transition:
    opacity 120ms ease,
    transform 120ms ease;
}

.admin-page-enter-from {
  opacity: 0;
  transform: translateY(4px);
}

.admin-page-leave-to {
  opacity: 0;
}

@media (max-width: 760px) {
  .admin-layout__aside {
    position: fixed;
    width: min(284px, calc(100vw - 48px)) !important;
    transform: translateX(-100%);

    &.is-mobile-open {
      transform: translateX(0);
    }
  }

  .admin-layout__aside .admin-layout__brand {
    justify-content: flex-start;
    padding: 0 14px;
  }

  .admin-layout__aside .admin-layout__brand-copy,
  .admin-layout__aside .admin-layout__aside-footer div {
    display: grid !important;
  }

  .admin-layout__aside .admin-layout__menu:not(.el-menu--collapse) {
    width: 100%;
  }

  .admin-layout__aside .admin-layout__menu.el-menu--collapse {
    width: 100%;
  }

  .admin-layout__aside .admin-layout__menu.el-menu--collapse :deep(.el-menu-item span),
  .admin-layout__aside .admin-layout__menu.el-menu--collapse :deep(.el-menu-item .el-menu-tooltip__trigger > span) {
    display: inline-block;
  }

  .admin-layout__aside .admin-layout__menu.el-menu--collapse :deep(.el-menu-item) {
    padding: 0 20px;
  }

  .admin-layout__aside-footer.is-collapsed {
    justify-content: flex-start;
    padding: 10px 12px;
  }

  .admin-layout__scrim {
    position: fixed;
    z-index: 29;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
    padding: 0;
    border: 0;
    background: rgb(15 23 42 / 42%);
    backdrop-filter: blur(2px);
  }

  .admin-layout__header {
    padding: 0 12px;
  }

  .admin-layout__desktop-toggle {
    display: none;
  }

  .admin-layout__mobile-toggle,
  .admin-layout__mobile-close {
    display: inline-flex;
  }

  .admin-layout__mobile-close {
    margin-left: auto;
  }

  .admin-layout__search-trigger {
    display: inline-flex;
    width: 38px;
    justify-content: center;
    padding: 0;
  }

  .admin-layout__search-trigger span,
  .admin-layout__search-trigger kbd,
  .admin-layout__account-copy,
  .admin-layout__account > .el-icon {
    display: none;
  }

  .admin-layout__account {
    min-height: 38px;
    padding: 1px;
  }
}

@media (max-width: 520px) {
  .admin-layout__title {
    max-width: 170px;
  }

  .admin-layout__eyebrow {
    font-size: 13px;
  }

  .admin-layout__header-actions {
    gap: 4px;
  }
}
</style>
