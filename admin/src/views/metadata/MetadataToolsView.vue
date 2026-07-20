<script setup lang="ts">
import { ElMessage, ElMessageBox } from 'element-plus';
import { computed, onMounted, reactive, ref } from 'vue';

import * as libraryApi from '@/services/library';
import * as metadataApi from '@/services/metadata';
import * as settingsApi from '@/services/settings';
import { useAuthStore } from '@/stores/auth';
import type { AdminMediaItem, MetadataItemType, ProviderIds, RemoteSearchResult } from '@/types/metadata';
import type { DoubanClientConfiguration, TmdbClientConfiguration } from '@/types/settings';

type IdentifyForm = {
  itemId: string;
  itemType: MetadataItemType;
  name: string;
  year?: number;
  tmdbId: string;
  doubanId: string;
};

const authStore = useAuthStore();
const loadingConfig = ref(false);
const searchingItems = ref(false);
const scanningLibrary = ref(false);
const resettingMetadata = ref(false);
const searchingRemote = ref(false);
const applyingIndex = ref<number | null>(null);
const applyingDirectIds = ref(false);
const imageVersion = ref(Date.now());
const searchTerm = ref('');
const itemTypeFilter = ref<MetadataItemType[]>(['Movie', 'Series']);
const items = ref<AdminMediaItem[]>([]);
const selectedIds = ref<string[]>([]);
const resetIdsText = ref('');
const resetMode = ref<'selected' | 'manual'>('selected');
const remoteResults = ref<RemoteSearchResult[]>([]);
const tmdbConfig = ref<TmdbClientConfiguration | null>(null);
const doubanConfig = ref<DoubanClientConfiguration | null>(null);

const itemTypeOptions: { label: string; value: MetadataItemType }[] = [
  { label: '电影', value: 'Movie' },
  { label: '剧集', value: 'Series' },
  { label: '人物', value: 'Person' }
];

const identifyForm = reactive<IdentifyForm>({
  itemId: '',
  itemType: 'Movie',
  name: '',
  year: undefined,
  tmdbId: '',
  doubanId: ''
});

const tmdbEnabled = computed(() => Boolean(tmdbConfig.value?.HasApiKey || tmdbConfig.value?.IsTmdbEnabled));
const tmdbHasProxy = computed(() => Boolean(tmdbConfig.value?.HasProxy));
const doubanHasCookie = computed(() => Boolean(doubanConfig.value?.HasCookie));
const manualResetIds = computed(() => parseItemIds(resetIdsText.value));
const resetIds = computed(() => (resetMode.value === 'selected' ? selectedIds.value : manualResetIds.value));
const selectedCount = computed(() => selectedIds.value.length);
const hasDirectProviderIds = computed(() => Boolean(identifyForm.tmdbId.trim() || identifyForm.doubanId.trim()));
const sourceStats = computed(() => [
  {
    label: 'TMDb',
    value: tmdbEnabled.value ? '已启用' : '未配置',
    hint: tmdbHasProxy.value ? '反代' : '元数据'
  },
  {
    label: '豆瓣',
    value: doubanHasCookie.value ? '已配置' : '匿名',
    hint: '中文资料'
  },
  {
    label: '已选择',
    value: selectedCount.value,
    hint: '条目'
  },
  {
    label: '识别结果',
    value: remoteResults.value.length,
    hint: '条'
  }
]);

async function loadConfiguration() {
  if (!authStore.token) {
    return;
  }

  loadingConfig.value = true;
  try {
    const [tmdb, douban] = await Promise.all([
      settingsApi.tmdbClientConfiguration(authStore.token),
      settingsApi.doubanClientConfiguration(authStore.token)
    ]);
    tmdbConfig.value = tmdb;
    doubanConfig.value = douban;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载元数据源失败');
  } finally {
    loadingConfig.value = false;
  }
}

async function searchLibraryItems() {
  if (!authStore.token) {
    return;
  }

  searchingItems.value = true;
  try {
    const result = await metadataApi.searchItems(authStore.token, searchTerm.value, itemTypeFilter.value);
    items.value = result.Items;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '搜索条目失败');
  } finally {
    searchingItems.value = false;
  }
}

async function triggerLibraryScan() {
  if (!authStore.token) {
    return;
  }

  scanningLibrary.value = true;
  try {
    await libraryApi.refreshLibrary(authStore.token);
    ElMessage.success('已触发媒体库扫描');
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '触发扫描失败');
  } finally {
    scanningLibrary.value = false;
  }
}

async function submitMetadataReset(rescan: boolean) {
  if (!authStore.token || resetIds.value.length === 0) {
    return;
  }

  const confirmed = await ElMessageBox.confirm(`确认重置 ${resetIds.value.length} 个条目的元数据？`, '重置元数据', {
    type: 'warning',
    confirmButtonText: rescan ? '重置并扫描' : '重置',
    cancelButtonText: '取消'
  })
    .then(() => true)
    .catch(() => false);
  if (!confirmed) {
    return;
  }

  resettingMetadata.value = true;
  try {
    await metadataApi.resetMetadata(authStore.token, resetIds.value);
    ElMessage.success('元数据已重置');
    imageVersion.value = Date.now();
    if (rescan) {
      await libraryApi.refreshLibrary(authStore.token);
      ElMessage.success('已触发重新扫描');
    }
    await searchLibraryItems();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '重置元数据失败');
  } finally {
    resettingMetadata.value = false;
  }
}

async function runRemoteSearch() {
  if (!authStore.token || !identifyForm.name.trim()) {
    return;
  }

  searchingRemote.value = true;
  try {
    remoteResults.value = await metadataApi.remoteSearch(authStore.token, {
      name: identifyForm.name.trim(),
      itemType: identifyForm.itemType,
      year: identifyForm.year,
      providerIds: identifyProviderIds()
    });
    if (remoteResults.value.length === 0) {
      ElMessage.info('没有找到匹配结果');
    }
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '远程识别失败');
  } finally {
    searchingRemote.value = false;
  }
}

async function applyResult(result: RemoteSearchResult, index: number) {
  if (!authStore.token || !identifyForm.itemId.trim()) {
    ElMessage.warning('请先选择或填写条目 ID');
    return;
  }

  applyingIndex.value = index;
  try {
    await metadataApi.applyRemoteSearch(authStore.token, identifyForm.itemId.trim(), result);
    ElMessage.success('识别结果已应用');
    imageVersion.value = Date.now();
    await searchLibraryItems();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '应用识别结果失败');
  } finally {
    applyingIndex.value = null;
  }
}

async function applyProviderIds() {
  if (!authStore.token || !identifyForm.itemId.trim() || !hasDirectProviderIds.value) {
    return;
  }

  applyingDirectIds.value = true;
  try {
    await metadataApi.applyRemoteSearch(authStore.token, identifyForm.itemId.trim(), {
      Name: identifyForm.name.trim() || 'Untitled',
      Type: identifyForm.itemType,
      ProductionYear: identifyForm.year ?? null,
      ProviderIds: identifyProviderIds()
    });
    ElMessage.success('外部 ID 已应用');
    imageVersion.value = Date.now();
    await searchLibraryItems();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '应用外部 ID 失败');
  } finally {
    applyingDirectIds.value = false;
  }
}

function identifyProviderIds(): ProviderIds {
  return {
    Tmdb: identifyForm.tmdbId.trim(),
    Douban: identifyForm.doubanId.trim()
  };
}

function selectItem(item: AdminMediaItem) {
  identifyForm.itemId = item.Id;
  identifyForm.name = item.Name;
  identifyForm.year = item.ProductionYear ?? undefined;
  identifyForm.itemType = normalizeItemType(item.Type);
  addSelectedId(item.Id);
}

function addSelectedId(itemId: string) {
  if (!selectedIds.value.includes(itemId)) {
    selectedIds.value = [...selectedIds.value, itemId];
  }
}

function setItemSelected(itemId: string, selected: boolean) {
  if (selected) {
    addSelectedId(itemId);
    return;
  }
  selectedIds.value = selectedIds.value.filter(id => id !== itemId);
}

function handleItemSelectionChange(itemId: string, value: string | number | boolean) {
  setItemSelected(itemId, Boolean(value));
}

function clearSelected() {
  selectedIds.value = [];
}

async function copyItemId(itemId: string) {
  try {
    await navigator.clipboard.writeText(itemId);
    ElMessage.success('ItemId 已复制');
  } catch {
    ElMessage.warning('复制失败');
  }
}

function useResultName(result: RemoteSearchResult) {
  identifyForm.name = result.Name;
  identifyForm.itemType = normalizeItemType(result.Type ?? identifyForm.itemType);
  identifyForm.year = result.ProductionYear ?? undefined;
  identifyForm.tmdbId = providerValue(result.ProviderIds, 'Tmdb');
  identifyForm.doubanId = providerValue(result.ProviderIds, 'Douban');
}

function parseItemIds(value: string) {
  return Array.from(
    new Set(
      value
        .split(/[\s,，;；]+/)
        .map(id => id.trim())
        .filter(Boolean)
    )
  );
}

function normalizeItemType(value?: string | null): MetadataItemType {
  if (value === 'Series' || value === 'Person') {
    return value;
  }
  return 'Movie';
}

function providerValue(providerIds: ProviderIds | undefined, provider: string) {
  const value = providerIds?.[provider];
  return value === null || value === undefined ? '' : String(value);
}

function providerSummary(providerIds?: ProviderIds) {
  const text = Object.entries(providerIds ?? {})
    .filter(([, value]) => value !== null && value !== undefined && value !== '')
    .map(([provider, value]) => `${provider}: ${String(value)}`)
    .join(' / ');
  return text || '-';
}

function itemTypeLabel(value?: string | null) {
  return itemTypeOptions.find(item => item.value === value)?.label ?? (value || '-');
}

function sourceLabel(value?: string | null) {
  if (!value) {
    return '本地';
  }
  if (value === 'TheMovieDb') {
    return 'TMDb';
  }
  return value;
}

function itemImageUrl(item: AdminMediaItem) {
  if (!authStore.token || !item.PrimaryImageTag) {
    return '';
  }
  return libraryApi.libraryPrimaryImageUrl(item.Id, authStore.token, imageVersion.value);
}

function hideImage(event: Event) {
  const image = event.target as HTMLImageElement;
  image.style.display = 'none';
}

onMounted(async () => {
  await Promise.all([loadConfiguration(), searchLibraryItems()]);
});
</script>

<template>
  <section class="admin-page metadata-page">
    <div class="metadata-page__heading">
      <div>
        <h1>元数据</h1>
        <p>扫描、重置与手动识别。</p>
      </div>
      <div class="metadata-page__heading-actions">
        <ElButton :loading="loadingConfig" @click="loadConfiguration">
          <ElIcon>
            <Refresh />
          </ElIcon>
          刷新状态
        </ElButton>
        <ElButton :loading="scanningLibrary" type="primary" @click="triggerLibraryScan">
          <ElIcon>
            <VideoPlay />
          </ElIcon>
          扫描媒体库
        </ElButton>
      </div>
    </div>

    <div v-loading="loadingConfig" class="metadata-page__stats">
      <div v-for="stat in sourceStats" :key="stat.label" class="metadata-page__stat">
        <span>{{ stat.label }}</span>
        <strong>{{ stat.value }}</strong>
        <small>{{ stat.hint }}</small>
      </div>
    </div>

    <div class="metadata-page__workspace">
      <article class="metadata-page__panel metadata-page__panel--items">
        <div class="metadata-page__panel-head metadata-page__panel-head--split">
          <div class="metadata-page__panel-title">
            <div class="metadata-page__panel-icon">
              <ElIcon>
                <Search />
              </ElIcon>
            </div>
            <div>
              <h2>库内条目</h2>
              <p>{{ items.length }} 条结果。</p>
            </div>
          </div>
          <ElButton :loading="searchingItems" @click="searchLibraryItems">
            <ElIcon>
              <Refresh />
            </ElIcon>
            刷新
          </ElButton>
        </div>

        <div class="metadata-page__searchbar">
          <ElInput
            v-model.trim="searchTerm"
            clearable
            placeholder="搜索标题"
            @clear="searchLibraryItems"
            @keyup.enter="searchLibraryItems"
          >
            <template #prefix>
              <ElIcon>
                <Search />
              </ElIcon>
            </template>
          </ElInput>
          <ElSelect v-model="itemTypeFilter" collapse-tags multiple placeholder="类型">
            <ElOption v-for="item in itemTypeOptions.slice(0, 2)" :key="item.value" :label="item.label" :value="item.value" />
          </ElSelect>
          <ElButton :loading="searchingItems" type="primary" @click="searchLibraryItems">搜索</ElButton>
        </div>

        <ElTable v-loading="searchingItems" :data="items" class="metadata-page__item-table" empty-text="暂无条目" height="420">
          <ElTableColumn width="48">
            <template #default="{ row }">
              <ElCheckbox
                :model-value="selectedIds.includes(row.Id)"
                @change="handleItemSelectionChange(row.Id, $event)"
              />
            </template>
          </ElTableColumn>
          <ElTableColumn label="条目" min-width="240">
            <template #default="{ row }">
              <div class="metadata-page__item-cell">
                <div class="metadata-page__poster metadata-page__poster--small">
                  <span>{{ row.Name.slice(0, 1).toUpperCase() }}</span>
                  <img :alt="row.Name" :src="itemImageUrl(row)" @error="hideImage" />
                </div>
                <div>
                  <strong>{{ row.Name }}</strong>
                  <span>{{ row.Id }}</span>
                </div>
              </div>
            </template>
          </ElTableColumn>
          <ElTableColumn label="类型" width="92">
            <template #default="{ row }">
              <ElTag effect="plain">{{ itemTypeLabel(row.Type) }}</ElTag>
            </template>
          </ElTableColumn>
          <ElTableColumn label="年份" width="86">
            <template #default="{ row }">{{ row.ProductionYear ?? '-' }}</template>
          </ElTableColumn>
          <ElTableColumn label="外部 ID" min-width="170">
            <template #default="{ row }">
              <span class="metadata-page__provider-text">{{ providerSummary(row.ProviderIds) }}</span>
            </template>
          </ElTableColumn>
          <ElTableColumn align="right" label="操作" width="154">
            <template #default="{ row }">
              <div class="metadata-page__row-actions">
                <ElButton text @click="copyItemId(row.Id)">
                  <ElIcon>
                    <CopyDocument />
                  </ElIcon>
                  复制
                </ElButton>
                <ElButton text type="primary" @click="selectItem(row)">
                  <ElIcon>
                    <Pointer />
                  </ElIcon>
                  选择
                </ElButton>
              </div>
            </template>
          </ElTableColumn>
        </ElTable>

        <div class="metadata-page__reset">
          <div class="metadata-page__reset-head">
            <ElSegmented
              v-model="resetMode"
              :options="[
                { label: '已选择', value: 'selected' },
                { label: '手动 ID', value: 'manual' }
              ]"
            />
            <ElButton :disabled="selectedIds.length === 0" text type="primary" @click="clearSelected">清空选择</ElButton>
          </div>
          <ElInput
            v-if="resetMode === 'manual'"
            v-model="resetIdsText"
            :autosize="{ minRows: 2, maxRows: 4 }"
            placeholder="ItemId，多个用换行或逗号分隔"
            type="textarea"
          />
          <div class="metadata-page__reset-actions">
            <span>{{ resetIds.length }} 个条目</span>
            <ElButton :disabled="resetIds.length === 0" :loading="resettingMetadata" @click="submitMetadataReset(false)">
              <ElIcon>
                <Delete />
              </ElIcon>
              重置元数据
            </ElButton>
            <ElButton
              :disabled="resetIds.length === 0"
              :loading="resettingMetadata"
              type="primary"
              @click="submitMetadataReset(true)"
            >
              <ElIcon>
                <RefreshRight />
              </ElIcon>
              重置后扫描
            </ElButton>
          </div>
        </div>
      </article>

      <article class="metadata-page__panel metadata-page__panel--identify">
        <div class="metadata-page__panel-head">
          <div class="metadata-page__panel-icon">
            <ElIcon>
              <MagicStick />
            </ElIcon>
          </div>
          <div>
            <h2>手动识别</h2>
            <p>{{ identifyForm.itemId || '未选择条目' }}</p>
          </div>
        </div>

        <ElForm class="metadata-page__identify-form" label-position="top">
          <ElFormItem label="条目 ID">
            <ElInput v-model.trim="identifyForm.itemId" placeholder="ItemId" />
          </ElFormItem>
          <div class="metadata-page__form-grid">
            <ElFormItem label="类型">
              <ElSelect v-model="identifyForm.itemType" class="metadata-page__control">
                <ElOption v-for="item in itemTypeOptions" :key="item.value" :label="item.label" :value="item.value" />
              </ElSelect>
            </ElFormItem>
            <ElFormItem label="年份">
              <ElInputNumber
                v-model="identifyForm.year"
                class="metadata-page__control"
                :max="2100"
                :min="1880"
                placeholder="年份"
              />
            </ElFormItem>
          </div>
          <ElFormItem label="名称">
            <ElInput v-model.trim="identifyForm.name" placeholder="标题" @keyup.enter="runRemoteSearch" />
          </ElFormItem>
          <div class="metadata-page__form-grid">
            <ElFormItem label="TMDb ID">
              <ElInput v-model.trim="identifyForm.tmdbId" clearable placeholder="12345" />
            </ElFormItem>
            <ElFormItem label="豆瓣 ID">
              <ElInput v-model.trim="identifyForm.doubanId" clearable placeholder="3541415" />
            </ElFormItem>
          </div>
          <div class="metadata-page__form-actions">
            <ElButton
              :disabled="!identifyForm.name.trim()"
              :loading="searchingRemote"
              type="primary"
              @click="runRemoteSearch"
            >
              <ElIcon>
                <Search />
              </ElIcon>
              远程搜索
            </ElButton>
            <ElButton
              :disabled="!identifyForm.itemId.trim() || !hasDirectProviderIds"
              :loading="applyingDirectIds"
              @click="applyProviderIds"
            >
              <ElIcon>
                <Check />
              </ElIcon>
              套用 ID
            </ElButton>
          </div>
        </ElForm>

        <div v-loading="searchingRemote" class="metadata-page__results">
          <article v-for="(result, index) in remoteResults" :key="`${result.SearchProviderName}-${index}`" class="metadata-page__result">
            <div class="metadata-page__poster">
              <span>{{ result.Name.slice(0, 1).toUpperCase() }}</span>
              <img v-if="result.ImageUrl" :alt="result.Name" :src="result.ImageUrl" @error="hideImage" />
            </div>
            <div class="metadata-page__result-main">
              <div class="metadata-page__result-head">
                <div>
                  <h3>{{ result.Name }}</h3>
                  <span>{{ itemTypeLabel(result.Type) }} · {{ result.ProductionYear ?? '-' }}</span>
                </div>
                <ElTag effect="plain" type="success">{{ sourceLabel(result.SearchProviderName) }}</ElTag>
              </div>
              <p>{{ result.Overview || '暂无简介' }}</p>
              <div class="metadata-page__result-meta">
                <span>{{ providerSummary(result.ProviderIds) }}</span>
                <span v-if="result.CommunityRating">评分 {{ result.CommunityRating }}</span>
              </div>
              <div class="metadata-page__result-actions">
                <ElButton @click="useResultName(result)">
                  <ElIcon>
                    <Edit />
                  </ElIcon>
                  填入
                </ElButton>
                <ElButton
                  :disabled="!identifyForm.itemId.trim()"
                  :loading="applyingIndex === index"
                  type="primary"
                  @click="applyResult(result, index)"
                >
                  <ElIcon>
                    <Check />
                  </ElIcon>
                  应用
                </ElButton>
              </div>
            </div>
          </article>

          <ElEmpty v-if="!searchingRemote && !remoteResults.length" :image-size="88" description="暂无识别结果" />
        </div>
      </article>
    </div>
  </section>
</template>

<style scoped lang="scss">
.metadata-page {
  display: grid;
  align-content: start;
  gap: 16px;
  padding: 24px 32px 32px;
}

.metadata-page__heading {
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

.metadata-page__heading-actions,
.metadata-page__form-actions,
.metadata-page__reset-actions,
.metadata-page__result-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  align-items: center;
}

.metadata-page__heading-actions {
  justify-content: flex-end;
}

.metadata-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 12px;
}

.metadata-page__stat {
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

.metadata-page__workspace {
  display: grid;
  grid-template-columns: minmax(0, 1.24fr) minmax(360px, 0.76fr);
  gap: 12px;
  align-items: start;
}

.metadata-page__panel {
  display: grid;
  gap: 16px;
  min-width: 0;
  padding: 16px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.04);
}

.metadata-page__panel--items {
  min-height: 640px;
}

.metadata-page__panel--identify {
  position: sticky;
  top: 80px;
}

.metadata-page__panel-head,
.metadata-page__panel-title {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.metadata-page__panel-head--split {
  justify-content: space-between;
}

.metadata-page__panel-icon {
  display: grid;
  width: 38px;
  height: 38px;
  flex: 0 0 38px;
  place-items: center;
  border-radius: 8px;
  color: #ffffff;
  background: linear-gradient(135deg, #0f766e, #2563eb);
}

.metadata-page__panel-head h2,
.metadata-page__result h3 {
  margin: 0;
  color: #0f172a;
  font-size: 17px;
  line-height: 1.25;
}

.metadata-page__panel-head p {
  margin: 4px 0 0;
  overflow: hidden;
  color: var(--admin-muted);
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.metadata-page__searchbar {
  display: grid;
  grid-template-columns: minmax(180px, 1fr) minmax(150px, 180px) auto;
  gap: 10px;
  align-items: center;
}

.metadata-page__item-table {
  width: 100%;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
}

.metadata-page__item-cell {
  display: grid;
  grid-template-columns: 42px minmax(0, 1fr);
  gap: 10px;
  align-items: center;
  min-width: 0;

  strong,
  span {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    color: #0f172a;
    font-size: 14px;
  }

  span {
    margin-top: 3px;
    color: var(--admin-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
    font-size: 12px;
  }
}

.metadata-page__provider-text {
  display: block;
  overflow: hidden;
  color: var(--admin-muted);
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.metadata-page__poster {
  position: relative;
  display: grid;
  width: 74px;
  height: 108px;
  flex: 0 0 74px;
  place-items: center;
  overflow: hidden;
  border-radius: 8px;
  color: #ffffff;
  background: linear-gradient(135deg, #334155, #0f766e);
  font-size: 22px;
  font-weight: 800;

  img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
}

.metadata-page__poster--small {
  width: 42px;
  height: 58px;
  flex-basis: 42px;
  font-size: 15px;
}

.metadata-page__reset {
  display: grid;
  gap: 12px;
  padding: 12px;
  border: 1px solid #d8e7e2;
  border-radius: 8px;
  background: #f8fbfa;
}

.metadata-page__reset-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: center;
}

.metadata-page__reset-actions {
  justify-content: flex-end;

  span {
    margin-right: auto;
    color: var(--admin-muted);
    font-size: 13px;
  }
}

.metadata-page__identify-form {
  display: grid;
  gap: 2px;
}

.metadata-page__form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.metadata-page__control {
  width: 100%;
}

.metadata-page__results {
  display: grid;
  align-content: start;
  gap: 12px;
  min-height: 260px;
}

.metadata-page__results :deep(.el-loading-mask),
.metadata-page__stats :deep(.el-loading-mask) {
  border-radius: 8px;
}

.metadata-page__result {
  display: flex;
  gap: 12px;
  min-width: 0;
  padding: 12px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
}

.metadata-page__result-main {
  display: grid;
  flex: 1;
  min-width: 0;
  gap: 8px;
}

.metadata-page__result-head {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  min-width: 0;

  div {
    min-width: 0;
  }

  h3,
  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    display: block;
    margin-top: 4px;
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.metadata-page__result p {
  display: -webkit-box;
  margin: 0;
  overflow: hidden;
  color: #475569;
  font-size: 13px;
  line-height: 1.55;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 3;
}

.metadata-page__result-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  color: var(--admin-muted);
  font-size: 12px;

  span {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.metadata-page__result-actions {
  justify-content: flex-end;
}

.metadata-page__row-actions {
  display: inline-flex;
  justify-content: flex-end;
  gap: 2px;
  white-space: nowrap;
}

@media (max-width: 1120px) {
  .metadata-page__workspace {
    grid-template-columns: 1fr;
  }

  .metadata-page__panel--identify {
    position: static;
  }
}

@media (max-width: 760px) {
  .metadata-page {
    padding: 18px;
  }

  .metadata-page__heading,
  .metadata-page__reset-head {
    display: grid;
  }

  .metadata-page__heading-actions,
  .metadata-page__reset-actions {
    justify-content: flex-start;
  }

  .metadata-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .metadata-page__searchbar,
  .metadata-page__form-grid {
    grid-template-columns: 1fr;
  }

  .metadata-page__result {
    display: grid;
  }

  .metadata-page__poster {
    width: 62px;
    height: 92px;
  }
}
</style>
