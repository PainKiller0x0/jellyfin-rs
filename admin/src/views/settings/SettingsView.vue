<script setup lang="ts">
import dayjs from 'dayjs';
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus';
import { computed, onMounted, reactive, ref } from 'vue';

import * as serverApi from '@/services/server';
import * as settingsApi from '@/services/settings';
import { useAuthStore } from '@/stores/auth';
import type { ApiKey, DoubanClientConfiguration, TmdbClientConfiguration } from '@/types/settings';

type ApiKeyForm = {
  appName: string;
};

const authStore = useAuthStore();
const loading = ref(false);
const savingServerName = ref(false);
const savingTmdb = ref(false);
const savingTmdbProxy = ref(false);
const savingDouban = ref(false);
const savingApiKey = ref(false);
const apiKeyDialogVisible = ref(false);
const serverName = ref('');
const tmdbApiKey = ref('');
const tmdbProxyUrl = ref('');
const doubanCookie = ref('');
const tmdbConfig = ref<TmdbClientConfiguration | null>(null);
const doubanConfig = ref<DoubanClientConfiguration | null>(null);
const keys = ref<ApiKey[]>([]);
const apiKeyFormRef = ref<FormInstance>();

const apiKeyForm = reactive<ApiKeyForm>({
  appName: ''
});

const apiKeyRules: FormRules<ApiKeyForm> = {
  appName: [{ required: true, message: '请输入应用名称', trigger: 'blur' }]
};

const tmdbEnabled = computed(() => Boolean(tmdbConfig.value?.HasApiKey || tmdbConfig.value?.IsTmdbEnabled));
const tmdbHasProxy = computed(() => Boolean(tmdbConfig.value?.HasProxy));
const doubanHasCookie = computed(() => Boolean(doubanConfig.value?.HasCookie));
const settingsStats = computed(() => [
  {
    label: '服务器',
    value: serverName.value || '-',
    hint: '名称'
  },
  {
    label: 'TMDb',
    value: tmdbEnabled.value ? '已启用' : '未配置',
    hint: tmdbHasProxy.value ? '代理' : '元数据'
  },
  {
    label: '豆瓣',
    value: doubanHasCookie.value ? '已配置' : '匿名',
    hint: '刮削'
  },
  {
    label: 'API 密钥',
    value: keys.value.length,
    hint: '个'
  }
]);

async function loadSettings() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    const [system, tmdb, douban, apiKeys] = await Promise.all([
      serverApi.systemInfo(authStore.token),
      settingsApi.tmdbClientConfiguration(authStore.token),
      settingsApi.doubanClientConfiguration(authStore.token),
      settingsApi.apiKeys(authStore.token)
    ]);
    serverName.value = system.ServerName;
    tmdbConfig.value = tmdb;
    tmdbProxyUrl.value = tmdb.ProxyUrl ?? tmdb.TmdbProxyUrl ?? '';
    doubanConfig.value = douban;
    keys.value = apiKeys.Items;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载配置失败');
  } finally {
    loading.value = false;
  }
}

async function saveServerName() {
  if (!authStore.token) {
    return;
  }

  savingServerName.value = true;
  try {
    const result = await serverApi.updateServerName(authStore.token, serverName.value);
    serverName.value = result.ServerName;
    ElMessage.success('服务器名称已保存');
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存服务器名称失败');
  } finally {
    savingServerName.value = false;
  }
}

async function saveTmdbApiKey() {
  if (!authStore.token) {
    return;
  }

  savingTmdb.value = true;
  try {
    await settingsApi.updateTmdbApiKey(authStore.token, tmdbApiKey.value);
    ElMessage.success('TMDb API Key 已保存');
    tmdbApiKey.value = '';
    await loadSettings();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存 TMDb API Key 失败');
  } finally {
    savingTmdb.value = false;
  }
}

async function saveTmdbProxyUrl() {
  if (!authStore.token) {
    return;
  }

  savingTmdbProxy.value = true;
  try {
    await settingsApi.updateTmdbProxyUrl(authStore.token, tmdbProxyUrl.value);
    ElMessage.success('TMDb 代理已保存');
    await loadSettings();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存 TMDb 代理失败');
  } finally {
    savingTmdbProxy.value = false;
  }
}

async function saveDoubanCookie() {
  if (!authStore.token) {
    return;
  }

  savingDouban.value = true;
  try {
    await settingsApi.updateDoubanCookie(authStore.token, doubanCookie.value);
    ElMessage.success('豆瓣 Cookie 已保存');
    doubanCookie.value = '';
    await loadSettings();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存豆瓣 Cookie 失败');
  } finally {
    savingDouban.value = false;
  }
}

function openApiKeyDialog() {
  apiKeyForm.appName = '';
  apiKeyFormRef.value?.clearValidate();
  apiKeyDialogVisible.value = true;
}

async function submitApiKey() {
  const formEl = apiKeyFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  savingApiKey.value = true;
  try {
    await settingsApi.createApiKey(authStore.token, apiKeyForm.appName);
    ElMessage.success('API 密钥已创建');
    apiKeyDialogVisible.value = false;
    await loadSettings();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '创建 API 密钥失败');
  } finally {
    savingApiKey.value = false;
  }
}

async function removeApiKey(row: ApiKey) {
  if (!authStore.token) {
    return;
  }

  const confirmed = await ElMessageBox.confirm(`确认删除 "${row.AppName}" 的 API 密钥？`, '删除 API 密钥', {
    type: 'warning',
    confirmButtonText: '删除',
    cancelButtonText: '取消'
  })
    .then(() => true)
    .catch(() => false);
  if (!confirmed) {
    return;
  }

  try {
    await settingsApi.deleteApiKey(authStore.token, row.AccessToken);
    ElMessage.success('API 密钥已删除');
    await loadSettings();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除 API 密钥失败');
  }
}

function formatDate(value?: string | null) {
  if (!value) {
    return '-';
  }

  const date = dayjs(value);
  return date.isValid() ? date.format('YYYY-MM-DD HH:mm') : value;
}

function maskToken(token: string) {
  if (token.length <= 12) {
    return token;
  }
  return `${token.slice(0, 6)}...${token.slice(-6)}`;
}

onMounted(loadSettings);
</script>

<template>
  <section class="admin-page settings-page">
    <div class="settings-page__heading">
      <div>
        <h1>设置</h1>
        <p>服务器基础信息、元数据源与访问密钥。</p>
      </div>
      <ElButton :loading="loading" type="primary" @click="loadSettings">
        <ElIcon>
          <Refresh />
        </ElIcon>
        刷新
      </ElButton>
    </div>

    <div class="settings-page__stats">
      <div v-for="stat in settingsStats" :key="stat.label" class="settings-page__stat">
        <span>{{ stat.label }}</span>
        <strong>{{ stat.value }}</strong>
        <small>{{ stat.hint }}</small>
      </div>
    </div>

    <div v-loading="loading" class="settings-page__config-grid">
      <article class="settings-page__panel">
        <div class="settings-page__panel-head">
          <div class="settings-page__panel-icon">
            <ElIcon>
              <Monitor />
            </ElIcon>
          </div>
          <div>
            <h2>服务器</h2>
            <p>后台与客户端显示名称。</p>
          </div>
        </div>

        <ElForm class="settings-page__form" label-position="top">
          <ElFormItem label="名称">
            <ElInput v-model.trim="serverName" maxlength="128" show-word-limit />
          </ElFormItem>
          <div class="settings-page__form-actions">
            <ElButton :loading="savingServerName" type="primary" @click="saveServerName">保存名称</ElButton>
          </div>
        </ElForm>
      </article>

      <article class="settings-page__panel settings-page__panel--metadata">
        <div class="settings-page__panel-head">
          <div class="settings-page__panel-icon">
            <ElIcon>
              <SetUp />
            </ElIcon>
          </div>
          <div>
            <h2>元数据源</h2>
            <p>配置 TMDb API Key 与豆瓣 Cookie。</p>
          </div>
        </div>

        <div class="settings-page__source-list">
          <section class="settings-page__source">
            <div class="settings-page__source-head">
              <div>
                <h3>TMDb</h3>
                <span>电影、剧集和人物元数据。</span>
              </div>
              <div class="settings-page__tag-group">
                <ElTag :type="tmdbEnabled ? 'success' : 'info'" effect="plain">
                  {{ tmdbEnabled ? '已启用' : '未配置' }}
                </ElTag>
                <ElTag :type="tmdbHasProxy ? 'success' : 'info'" effect="plain">
                  {{ tmdbHasProxy ? '代理已配置' : '直连' }}
                </ElTag>
              </div>
            </div>

            <div class="settings-page__inline-control">
              <ElInput v-model.trim="tmdbApiKey" placeholder="TMDb API Key" show-password type="password" />
              <ElButton :loading="savingTmdb" type="primary" @click="saveTmdbApiKey">保存</ElButton>
            </div>
            <div class="settings-page__inline-control">
              <ElInput v-model.trim="tmdbProxyUrl" clearable maxlength="2048" placeholder="http://host.docker.internal:7890" />
              <ElButton :loading="savingTmdbProxy" type="primary" @click="saveTmdbProxyUrl">保存代理</ElButton>
            </div>
          </section>

          <section class="settings-page__source">
            <div class="settings-page__source-head">
              <div>
                <h3>豆瓣</h3>
                <span>中文资料与评分补充。</span>
              </div>
              <ElTag :type="doubanHasCookie ? 'success' : 'info'" effect="plain">
                {{ doubanHasCookie ? '已配置 Cookie' : '匿名模式' }}
              </ElTag>
            </div>

            <ElInput
              v-model="doubanCookie"
              maxlength="16384"
              placeholder="dbcl2=...; ck=..."
              show-password
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 3 }"
            />
            <div class="settings-page__form-actions">
              <ElButton :loading="savingDouban" type="primary" @click="saveDoubanCookie">保存 Cookie</ElButton>
            </div>
          </section>
        </div>
      </article>

      <article class="settings-page__panel settings-page__panel--keys">
        <div class="settings-page__panel-head settings-page__panel-head--split">
          <div class="settings-page__panel-title">
            <div class="settings-page__panel-icon">
              <ElIcon>
                <Key />
              </ElIcon>
            </div>
            <div>
              <h2>API 密钥</h2>
              <p>{{ keys.length }} 个应用密钥。</p>
            </div>
          </div>
          <ElButton type="primary" @click="openApiKeyDialog">
            <ElIcon>
              <Plus />
            </ElIcon>
            新建密钥
          </ElButton>
        </div>

        <div class="settings-page__key-list">
          <div v-for="key in keys" :key="key.Id || key.AccessToken" class="settings-page__key-row">
            <div class="settings-page__key-app">
              <strong>{{ key.AppName }}</strong>
              <ElTooltip :content="key.AccessToken" placement="top">
                <span class="settings-page__token">{{ maskToken(key.AccessToken) }}</span>
              </ElTooltip>
            </div>
            <div class="settings-page__key-meta">
              <span>创建 {{ formatDate(key.DateCreated) }}</span>
              <span>最近使用 {{ formatDate(key.DateLastActivity) }}</span>
            </div>
            <ElButton type="danger" @click="removeApiKey(key)">
              <ElIcon>
                <Delete />
              </ElIcon>
              删除
            </ElButton>
          </div>

          <ElEmpty v-if="!loading && !keys.length" :image-size="88" description="暂无 API 密钥">
            <ElButton type="primary" @click="openApiKeyDialog">
              <ElIcon>
                <Plus />
              </ElIcon>
              新建密钥
            </ElButton>
          </ElEmpty>
        </div>
      </article>
    </div>

    <ElDialog v-model="apiKeyDialogVisible" title="新建 API 密钥" width="440px">
      <ElForm ref="apiKeyFormRef" :model="apiKeyForm" :rules="apiKeyRules" label-position="top">
        <ElFormItem label="应用名称" prop="appName">
          <ElInput v-model.trim="apiKeyForm.appName" />
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="apiKeyDialogVisible = false">取消</ElButton>
        <ElButton :loading="savingApiKey" type="primary" @click="submitApiKey">保存</ElButton>
      </template>
    </ElDialog>
  </section>
</template>

<style scoped lang="scss">
.settings-page {
  display: grid;
  align-content: start;
  gap: 16px;
  padding: 24px 32px 32px;
}

.settings-page__heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;

  h1 {
    margin: 0;
    font-size: 24px;
    line-height: 1.25;
  }

  p {
    margin: 6px 0 0;
    color: var(--admin-muted);
    font-size: 14px;
  }
}

.settings-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 12px;
}

.settings-page__stat {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 6px 10px;
  align-items: baseline;
  min-height: 58px;
  padding: 12px 14px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;

  span {
    min-width: 0;
    overflow: hidden;
    color: var(--admin-muted);
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    overflow: hidden;
    color: #0f766e;
    font-size: 20px;
    line-height: 1.15;
    text-align: right;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  small {
    grid-column: 1 / -1;
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.settings-page__config-grid {
  display: grid;
  grid-template-columns: minmax(280px, 0.72fr) minmax(0, 1.28fr);
  gap: 12px;
  align-items: start;
}

.settings-page__config-grid :deep(.el-loading-mask) {
  border-radius: 8px;
}

.settings-page__panel {
  display: grid;
  gap: 16px;
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.04);
}

.settings-page__panel--metadata {
  grid-row: span 2;
}

.settings-page__panel--keys {
  grid-column: 1 / -1;
}

.settings-page__panel-head,
.settings-page__panel-title {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.settings-page__panel-head {
  h2,
  p {
    margin: 0;
  }

  h2 {
    color: #0f172a;
    font-size: 17px;
    line-height: 1.25;
  }

  p {
    margin-top: 4px;
    color: var(--admin-muted);
    font-size: 13px;
  }
}

.settings-page__panel-head--split {
  justify-content: space-between;
}

.settings-page__panel-icon {
  display: grid;
  width: 38px;
  height: 38px;
  flex: 0 0 38px;
  place-items: center;
  border-radius: 8px;
  color: #0f766e;
  background: #e6f4f1;
  font-size: 18px;
}

.settings-page__form :deep(.el-form-item) {
  margin-bottom: 14px;
}

.settings-page__form :deep(.el-form-item__label) {
  padding-bottom: 7px;
  color: #334155;
  font-weight: 700;
}

.settings-page__form-actions {
  display: flex;
  justify-content: flex-end;
}

.settings-page__source-list {
  display: grid;
  gap: 12px;
}

.settings-page__source {
  display: grid;
  gap: 10px;
  padding: 12px;
  border: 1px solid #edf2f7;
  border-radius: 8px;
  background: #fbfdff;
}

.settings-page__source-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  min-width: 0;

  h3,
  span {
    margin: 0;
  }

  h3 {
    color: #1f2937;
    font-size: 15px;
    line-height: 1.25;
  }

  span {
    display: block;
    margin-top: 3px;
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.settings-page__tag-group {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}

.settings-page__inline-control {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 10px;
}

.settings-page__key-list {
  display: grid;
  gap: 8px;
}

.settings-page__key-row {
  display: grid;
  grid-template-columns: minmax(180px, 0.8fr) minmax(260px, 1fr) auto;
  gap: 12px;
  align-items: center;
  min-height: 54px;
  padding: 10px 12px;
  border: 1px solid #edf2f7;
  border-radius: 8px;
  background: #fbfdff;
}

.settings-page__key-app {
  display: grid;
  min-width: 0;
  gap: 4px;

  strong {
    overflow: hidden;
    color: #0f172a;
    font-size: 14px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.settings-page__key-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  min-width: 0;

  span {
    min-height: 28px;
    padding: 6px 9px;
    border: 1px solid #e2e8f0;
    border-radius: 8px;
    color: #475569;
    background: #f8fafc;
    font-size: 12px;
    line-height: 1.2;
  }
}

.settings-page__token {
  overflow: hidden;
  color: var(--admin-muted);
  font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace;
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (max-width: 820px) {
  .settings-page {
    padding: 18px;
  }

  .settings-page__heading {
    flex-direction: column;
  }

  .settings-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .settings-page__config-grid,
  .settings-page__key-row {
    grid-template-columns: 1fr;
  }

  .settings-page__panel--metadata,
  .settings-page__panel--keys {
    grid-column: auto;
    grid-row: auto;
  }

  .settings-page__panel-head--split {
    align-items: flex-start;
  }
}

@media (max-width: 540px) {
  .settings-page__stats,
  .settings-page__inline-control {
    grid-template-columns: 1fr;
  }

  .settings-page__panel-head--split,
  .settings-page__source-head {
    display: grid;
  }

  .settings-page__tag-group {
    justify-content: flex-start;
  }

  .settings-page__form-actions {
    justify-content: flex-start;
  }
}
</style>
