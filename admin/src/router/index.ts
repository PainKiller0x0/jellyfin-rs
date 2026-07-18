import NProgress from 'nprogress';
import 'nprogress/nprogress.css';
import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';

import AdminLayout from '@/layouts/AdminLayout.vue';
import { useAuthStore } from '@/stores/auth';

const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/views/login/LoginView.vue'),
    meta: { title: '登录' }
  },
  {
    path: '/',
    component: AdminLayout,
    redirect: '/dashboard',
    meta: { requiresAuth: true },
    children: [
      {
        path: 'dashboard',
        name: 'dashboard',
        component: () => import('@/views/dashboard/DashboardView.vue'),
        meta: { title: '控制台', icon: 'DataLine' }
      },
      {
        path: 'libraries',
        name: 'libraries',
        component: () => import('@/views/library/LibraryView.vue'),
        meta: { title: '媒体库', icon: 'FolderOpened' }
      },
      {
        path: 'tasks',
        name: 'tasks',
        component: () => import('@/views/tasks/TasksView.vue'),
        meta: { title: '计划任务', icon: 'Clock' }
      },
      {
        path: 'users',
        name: 'users',
        component: () => import('@/views/users/UsersView.vue'),
        meta: { title: '用户', icon: 'User' }
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/views/settings/SettingsView.vue'),
        meta: { title: '设置', icon: 'Setting' }
      }
    ]
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('@/views/error/NotFoundView.vue'),
    meta: { title: '页面不存在' }
  }
];

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes
});

NProgress.configure({ showSpinner: false });

router.beforeEach(async to => {
  NProgress.start();
  document.title = `${String(to.meta.title ?? '管理后台')} - jellyfin-rs`;

  const authStore = useAuthStore();
  authStore.restore();

  if (authStore.isAuthenticated && !authStore.user) {
    await authStore.fetchMe().catch(async () => {
      await authStore.logout();
    });
  }

  if (to.name === 'login' && authStore.isAuthenticated) {
    return { path: '/' };
  }

  if (to.matched.some(record => record.meta.requiresAuth) && !authStore.isAuthenticated) {
    return {
      path: '/login',
      query: {
        redirect: to.fullPath
      }
    };
  }
});

router.afterEach(() => {
  NProgress.done();
});

export default router;
