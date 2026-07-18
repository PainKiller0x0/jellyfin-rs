import NProgress from 'nprogress';
import 'nprogress/nprogress.css';
import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';

import AdminLayout from '@/layouts/AdminLayout.vue';

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    component: AdminLayout,
    redirect: '/dashboard',
    children: [
      {
        path: 'dashboard',
        name: 'dashboard',
        component: () => import('@/views/dashboard/DashboardView.vue'),
        meta: { title: '控制台', icon: 'DataLine' }
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

router.beforeEach(to => {
  NProgress.start();
  document.title = `${String(to.meta.title ?? '管理后台')} - jellyfin-rs`;
});

router.afterEach(() => {
  NProgress.done();
});

export default router;
