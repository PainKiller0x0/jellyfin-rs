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
const savingDouban = ref(false);
const savingApiKey = ref(false);
const apiKeyDialogVisible = ref(false);
const serverName = ref('');
const tmdbApiKey = ref('');
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
const doubanHasCookie = computed(() => Boolean(doubanConfig.value?.HasCookie));

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

    <div class="settings-page__grid">
      <ElCard shadow="never">
        <template #header>
          <div class="settings-page__card-title">
            <ElIcon>
              <Monitor />
            </ElIcon>
            <span>服务器</span>
          </div>
        </template>

        <ElForm label-position="top">
          <ElFormItem label="名称">
            <ElInput v-model.trim="serverName" maxlength="128" show-word-limit />
          </ElFormItem>
          <ElButton :loading="savingServerName" type="primary" @click="saveServerName">保存</ElButton>
        </ElForm>
      </ElCard>

      <ElCard shadow="never">
        <template #header>
          <div class="settings-page__card-title">
            <ElIcon>
              <SetUp />
            </ElIcon>
            <span>豆瓣</span>
            <ElTag :type="doubanHasCookie ? 'success' : 'info'" effect="plain">
              {{ doubanHasCookie ? '已配置 Cookie' : '匿名模式' }}
            </ElTag>
          </div>
        </template>

        <ElForm label-position="top">
          <ElFormItem label="Cookie">
            <ElInput
              v-model="doubanCookie"
              maxlength="16384"
              placeholder="dbcl2=...; ck=..."
              show-password
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 4 }"
            />
          </ElFormItem>
          <ElButton :loading="savingDouban" type="primary" @click="saveDoubanCookie">保存</ElButton>
        </ElForm>
      </ElCard>

      <ElCard shadow="never">
        <template #header>
          <div class="settings-page__card-title">
            <ElIcon>
              <SetUp />
            </ElIcon>
            <span>TMDb</span>
            <ElTag :type="tmdbEnabled ? 'success' : 'info'" effect="plain">
              {{ tmdbEnabled ? '已启用' : '未配置' }}
            </ElTag>
          </div>
        </template>

        <ElForm label-position="top">
          <ElFormItem label="API Key">
            <ElInput v-model.trim="tmdbApiKey" placeholder="TMDb API Key" show-password type="password" />
          </ElFormItem>
          <ElButton :loading="savingTmdb" type="primary" @click="saveTmdbApiKey">保存</ElButton>
        </ElForm>
      </ElCard>

      <ElCard shadow="never">
        <template #header>
          <div class="settings-page__card-title">
            <ElIcon>
              <Key />
            </ElIcon>
            <span>API 密钥</span>
            <ElTag effect="plain">{{ keys.length }}</ElTag>
          </div>
        </template>

        <ElButton type="primary" @click="openApiKeyDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建密钥
        </ElButton>
      </ElCard>
    </div>

    <ElCard shadow="never">
      <ElTable v-loading="loading" :data="keys" empty-text="暂无 API 密钥">
        <ElTableColumn label="应用" min-width="160" prop="AppName" />
        <ElTableColumn label="Token" min-width="190">
          <template #default="{ row }">
            <ElTooltip :content="row.AccessToken" placement="top">
              <span class="settings-page__token">{{ maskToken(row.AccessToken) }}</span>
            </ElTooltip>
          </template>
        </ElTableColumn>
        <ElTableColumn label="创建时间" min-width="150">
          <template #default="{ row }">
            {{ formatDate(row.DateCreated) }}
          </template>
        </ElTableColumn>
        <ElTableColumn label="最近使用" min-width="150">
          <template #default="{ row }">
            {{ formatDate(row.DateLastActivity) }}
          </template>
        </ElTableColumn>
        <ElTableColumn align="right" label="操作" width="100">
          <template #default="{ row }">
            <ElButton link type="danger" @click="removeApiKey(row)">删除</ElButton>
          </template>
        </ElTableColumn>
      </ElTable>
    </ElCard>

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
  gap: 18px;
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

.settings-page__grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 16px;
}

.settings-page__card-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 700;
}

.settings-page__token {
  font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace;
}

@media (max-width: 820px) {
  .settings-page__heading {
    flex-direction: column;
  }

  .settings-page__grid {
    grid-template-columns: 1fr;
  }
}
</style>
