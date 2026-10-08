import type { RouteRecordRaw } from 'vue-router'

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/views/login/index.vue'),
  },
  {
    path: '/key-usage',
    name: 'key-usage',
    component: () => import('@/plugins/PluginRoute.vue'),
  },
  {
    path: '/',
    component: () => import('@/layout/index.vue'),
    children: [
      { path: 'extensions/:plugin/:page?', component: () => import('@/plugins/PluginRoute.vue') },
      {
        path: '',
        name: 'dashboard',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'accounts',
        name: 'accounts',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'proxies',
        name: 'proxies',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'groups',
        name: 'groups',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'keys',
        name: 'keys',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'usage',
        name: 'usage',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'theme',
        name: 'theme',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'settings/upstream',
        name: 'settings-upstream',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'settings/access',
        name: 'settings-access',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'settings/backup',
        name: 'settings-backup',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
      {
        path: 'settings/plugins',
        name: 'settings-plugins',
        component: () => import('@/plugins/PluginManager.vue'),
      },
      {
        path: 'settings/pricing',
        name: 'settings-pricing',
        component: () => import('@/plugins/PluginRoute.vue'),
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
