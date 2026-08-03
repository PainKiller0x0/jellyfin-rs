<script setup lang="ts">
import { ElMessage, ElMessageBox, type FormInstance, type FormRules, type UploadFile } from 'element-plus';
import { computed, onMounted, reactive, ref } from 'vue';

import DirectoryPicker from '@/components/DirectoryPicker.vue';
import * as libraryApi from '@/services/library';
import { useAuthStore } from '@/stores/auth';
import type { VirtualFolder } from '@/types/library';

type LibraryForm = {
  name: string;
  collectionType: string;
  pathsText: string;
};

type PathForm = {
  name: string;
  path: string;
};

const authStore = useAuthStore();
const loading = ref(false);
const saving = ref(false);
const imageVersion = ref(Date.now());
const uploadingImageId = ref('');
const deletingImageId = ref('');
const folders = ref<VirtualFolder[]>([]);
const createDialogVisible = ref(false);
const pathDialogVisible = ref(false);
const directoryPickerVisible = ref(false);
const directoryPickerTarget = ref<'create' | 'path'>('create');
const formRef = ref<FormInstance>();
const pathFormRef = ref<FormInstance>();

const collectionTypes = [
  { label: '电影', value: 'movies' },
  { label: '剧集', value: 'tvshows' },
  { label: '音乐', value: 'music' },
  { label: '混合内容', value: 'mixed' }
];

const form = reactive<LibraryForm>({
  name: '',
  collectionType: 'movies',
  pathsText: ''
});

const pathForm = reactive<PathForm>({
  name: '',
  path: ''
});

const rules: FormRules<LibraryForm> = {
  name: [{ required: true, message: '请输入媒体库名称', trigger: 'blur' }],
  collectionType: [{ required: true, message: '请选择媒体类型', trigger: 'change' }]
};

const pathRules: FormRules<PathForm> = {
  name: [{ required: true, message: '请选择媒体库', trigger: 'change' }],
  path: [{ required: true, message: '请输入媒体路径', trigger: 'blur' }]
};

const totalPaths = computed(() => folders.value.reduce((sum, folder) => sum + folder.Locations.length, 0));
const createPathRows = computed(() => parsePaths(form.pathsText));
const typeCount = computed(() =>
  folders.value.reduce<Record<string, number>>((counts, folder) => {
    const key = folder.CollectionType || 'mixed';
    counts[key] = (counts[key] ?? 0) + 1;
    return counts;
  }, {})
);
const libraryStats = computed(() => [
  {
    label: '媒体库',
    value: folders.value.length,
    hint: '个'
  },
  {
    label: '媒体路径',
    value: totalPaths.value,
    hint: '条'
  },
  {
    label: '电影库',
    value: typeCount.value.movies ?? 0,
    hint: '个'
  },
  {
    label: '剧集库',
    value: typeCount.value.tvshows ?? 0,
    hint: '个'
  }
]);

async function loadFolders() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    folders.value = await libraryApi.virtualFolders(authStore.token);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载媒体库失败');
  } finally {
    loading.value = false;
  }
}

function resetForm() {
  form.name = '';
  form.collectionType = 'movies';
  form.pathsText = '';
  formRef.value?.clearValidate();
}

function resetPathForm(folder?: VirtualFolder) {
  pathForm.name = folder?.Name ?? '';
  pathForm.path = '';
  pathFormRef.value?.clearValidate();
}

function parsePaths(value: string) {
  return value
    .split('\n')
    .map(path => path.trim())
    .filter(Boolean);
}

function appendCreatePath(path: string) {
  const paths = parsePaths(form.pathsText);
  if (!paths.includes(path)) {
    paths.push(path);
  }
  form.pathsText = paths.join('\n');
}

function removeCreatePath(path: string) {
  form.pathsText = parsePaths(form.pathsText)
    .filter(item => item !== path)
    .join('\n');
}

async function submitCreate() {
  const formEl = formRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  saving.value = true;
  try {
    await libraryApi.createVirtualFolder(authStore.token, {
      name: form.name,
      collectionType: form.collectionType,
      paths: parsePaths(form.pathsText)
    });
    ElMessage.success('媒体库已保存');
    createDialogVisible.value = false;
    resetForm();
    await loadFolders();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存媒体库失败');
  } finally {
    saving.value = false;
  }
}

async function submitPath() {
  const formEl = pathFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  saving.value = true;
  try {
    await libraryApi.addLibraryPath(authStore.token, pathForm);
    ElMessage.success('路径已添加');
    pathDialogVisible.value = false;
    resetPathForm();
    await loadFolders();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '添加路径失败');
  } finally {
    saving.value = false;
  }
}

async function removeFolder(folder: VirtualFolder) {
  if (!authStore.token) {
    return;
  }

  const confirmed = await ElMessageBox.confirm(`确认删除媒体库 "${folder.Name}"？`, '删除媒体库', {
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
    await libraryApi.deleteVirtualFolder(authStore.token, folder.Name);
    ElMessage.success('媒体库已删除');
    await loadFolders();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除媒体库失败');
  }
}

async function removePath(folder: VirtualFolder, path: string) {
  if (!authStore.token) {
    return;
  }

  try {
    await libraryApi.deleteLibraryPath(authStore.token, {
      name: folder.Name,
      path
    });
    ElMessage.success('路径已删除');
    await loadFolders();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除路径失败');
  }
}

async function scanLibrary() {
  if (!authStore.token) {
    return;
  }

  try {
    const result = await libraryApi.refreshLibrary(authStore.token);
    if (result.AlreadyRunning) {
      ElMessage.info('媒体库扫描已在运行');
    } else {
      ElMessage.success('已触发媒体库扫描');
    }
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '触发扫描失败');
  }
}

function folderImageUrl(folder: VirtualFolder) {
  if (!authStore.token) {
    return '';
  }
  return libraryApi.libraryPrimaryImageUrl(folder.ItemId, authStore.token, imageVersion.value);
}

function showFolderImage(event: Event) {
  const image = event.target as HTMLImageElement;
  image.style.display = 'block';
}

function hideFolderImage(event: Event) {
  const image = event.target as HTMLImageElement;
  image.style.display = 'none';
}

function collectionTypeLabel(value: string) {
  return collectionTypes.find(item => item.value === value)?.label ?? (value || '混合内容');
}

function collectionTypeIcon(value: string) {
  if (value === 'movies') {
    return 'Film';
  }
  if (value === 'tvshows') {
    return 'VideoCamera';
  }
  if (value === 'music') {
    return 'Headset';
  }
  return 'Collection';
}

function collectionTypeClass(value: string) {
  if (value === 'movies' || value === 'tvshows' || value === 'music') {
    return `library-page__cover--${value}`;
  }
  return 'library-page__cover--mixed';
}

function coverInitial(folder: VirtualFolder) {
  return folder.Name.slice(0, 1).toUpperCase();
}

async function uploadFolderImage(folder: VirtualFolder, uploadFile: UploadFile) {
  if (!authStore.token || !uploadFile.raw) {
    return;
  }

  uploadingImageId.value = folder.ItemId;
  try {
    await libraryApi.uploadLibraryPrimaryImage(authStore.token, folder.ItemId, uploadFile.raw);
    imageVersion.value = Date.now();
    ElMessage.success('媒体库图片已保存');
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '保存媒体库图片失败');
  } finally {
    uploadingImageId.value = '';
  }
}

function folderImageChangeHandler(folder: VirtualFolder) {
  return (uploadFile: UploadFile) => {
    void uploadFolderImage(folder, uploadFile);
  };
}

async function deleteFolderImage(folder: VirtualFolder) {
  if (!authStore.token) {
    return;
  }

  deletingImageId.value = folder.ItemId;
  try {
    await libraryApi.deleteLibraryPrimaryImage(authStore.token, folder.ItemId);
    imageVersion.value = Date.now();
    ElMessage.success('媒体库图片已删除');
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除媒体库图片失败');
  } finally {
    deletingImageId.value = '';
  }
}

function openCreateDialog() {
  resetForm();
  createDialogVisible.value = true;
}

function openPathDialog(folder?: VirtualFolder) {
  resetPathForm(folder);
  pathDialogVisible.value = true;
}

function openDirectoryPicker(target: 'create' | 'path') {
  directoryPickerTarget.value = target;
  directoryPickerVisible.value = true;
}

function handleDirectorySelect(path: string) {
  if (directoryPickerTarget.value === 'create') {
    appendCreatePath(path);
  } else {
    pathForm.path = path;
  }
}

onMounted(loadFolders);
</script>

<template>
  <section class="admin-page library-page">
    <div class="library-page__heading">
      <div>
        <h1>媒体库</h1>
        <p>{{ folders.length }} 个媒体库，{{ totalPaths }} 个路径。</p>
      </div>
      <div class="library-page__heading-actions">
        <ElButton :loading="loading" @click="loadFolders">
          <ElIcon>
            <Refresh />
          </ElIcon>
          刷新
        </ElButton>
        <ElButton @click="scanLibrary">
          <ElIcon>
            <VideoPlay />
          </ElIcon>
          扫描
        </ElButton>
        <ElButton @click="openPathDialog()">
          <ElIcon>
            <FolderAdd />
          </ElIcon>
          添加路径
        </ElButton>
        <ElButton type="primary" @click="openCreateDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建媒体库
        </ElButton>
      </div>
    </div>

    <div class="library-page__stats">
      <div v-for="stat in libraryStats" :key="stat.label" class="library-page__stat">
        <span>{{ stat.label }}</span>
        <strong>{{ stat.value }}</strong>
        <small>{{ stat.hint }}</small>
      </div>
    </div>

    <div v-loading="loading" class="library-page__library-list">
      <article v-for="folder in folders" :key="folder.Id" class="library-page__library-card">
        <div class="library-page__library-main">
          <div class="library-page__cover" :class="collectionTypeClass(folder.CollectionType)">
            <span>{{ coverInitial(folder) }}</span>
            <img
              :alt="folder.Name"
              :src="folderImageUrl(folder)"
              @error="hideFolderImage"
              @load="showFolderImage"
            />
          </div>

          <div class="library-page__library-copy">
            <div class="library-page__library-title">
              <div>
                <h2>{{ folder.Name }}</h2>
                <span>{{ folder.ItemId }}</span>
              </div>
              <ElTag effect="plain" type="success">{{ collectionTypeLabel(folder.CollectionType) }}</ElTag>
            </div>

            <div class="library-page__paths">
              <div v-for="path in folder.Locations" :key="path" class="library-page__path-pill">
                <ElIcon>
                  <Folder />
                </ElIcon>
                <span>{{ path }}</span>
                <ElButton circle text type="danger" @click="removePath(folder, path)">
                  <ElIcon>
                    <Close />
                  </ElIcon>
                </ElButton>
              </div>
              <ElButton class="library-page__path-add" text type="primary" @click="openPathDialog(folder)">
                <ElIcon>
                  <Plus />
                </ElIcon>
                路径
              </ElButton>
            </div>
          </div>
        </div>

        <div class="library-page__card-actions">
          <ElButton @click="openPathDialog(folder)">
            <ElIcon>
              <FolderAdd />
            </ElIcon>
            添加路径
          </ElButton>
          <ElUpload
            accept="image/jpeg,image/png,image/webp"
            :auto-upload="false"
            :on-change="folderImageChangeHandler(folder)"
            :show-file-list="false"
          >
            <ElButton :loading="uploadingImageId === folder.ItemId">
              <ElIcon>
                <Upload />
              </ElIcon>
              上传封面
            </ElButton>
          </ElUpload>
          <ElButton :loading="deletingImageId === folder.ItemId" @click="deleteFolderImage(folder)">
            <ElIcon>
              <Picture />
            </ElIcon>
            删除封面
          </ElButton>
          <ElButton type="danger" @click="removeFolder(folder)">
            <ElIcon>
              <Delete />
            </ElIcon>
            删除
          </ElButton>
        </div>
      </article>

      <ElEmpty v-if="!loading && !folders.length" :image-size="96" description="暂无媒体库">
        <ElButton type="primary" @click="openCreateDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建媒体库
        </ElButton>
      </ElEmpty>
    </div>

    <ElDialog
      v-model="createDialogVisible"
      class="library-page__create-dialog"
      width="min(660px, calc(100vw - 32px))"
      @closed="resetForm"
    >
      <template #header>
        <div class="library-page__dialog-header">
          <div class="library-page__dialog-icon">
            <ElIcon>
              <FolderOpened />
            </ElIcon>
          </div>
          <h2>新建媒体库</h2>
        </div>
      </template>

      <ElForm ref="formRef" class="library-page__create-form" :model="form" :rules="rules" label-position="top">
        <ElFormItem label="名称" prop="name">
          <ElInput v-model.trim="form.name" placeholder="电影" size="large" />
        </ElFormItem>

        <ElFormItem label="媒体类型" prop="collectionType">
          <div class="library-page__type-grid">
            <button
              v-for="item in collectionTypes"
              :key="item.value"
              class="library-page__type-option"
              :class="{ 'is-active': form.collectionType === item.value }"
              type="button"
              @click="form.collectionType = item.value"
            >
              <ElIcon>
                <component :is="collectionTypeIcon(item.value)" />
              </ElIcon>
              <strong>{{ item.label }}</strong>
              <span>{{ item.value }}</span>
            </button>
          </div>
        </ElFormItem>

        <ElFormItem label="路径">
          <div class="library-page__create-paths">
            <div class="library-page__create-path-input">
              <ElInput v-model="form.pathsText" :rows="3" placeholder="/media/Movies" type="textarea" />
              <ElButton @click="openDirectoryPicker('create')">
                <ElIcon>
                  <FolderOpened />
                </ElIcon>
                选择
              </ElButton>
            </div>

            <div v-if="createPathRows.length" class="library-page__create-path-list">
              <div v-for="path in createPathRows" :key="path" class="library-page__create-path-row">
                <ElIcon>
                  <Folder />
                </ElIcon>
                <span>{{ path }}</span>
                <ElButton circle text type="danger" @click="removeCreatePath(path)">
                  <ElIcon>
                    <Close />
                  </ElIcon>
                </ElButton>
              </div>
            </div>
          </div>
        </ElFormItem>
      </ElForm>

      <template #footer>
        <div class="library-page__dialog-footer">
          <ElButton @click="createDialogVisible = false">取消</ElButton>
          <ElButton :loading="saving" type="primary" @click="submitCreate">保存</ElButton>
        </div>
      </template>
    </ElDialog>

    <ElDialog v-model="pathDialogVisible" title="添加路径" width="520px" @closed="resetPathForm()">
      <ElForm ref="pathFormRef" :model="pathForm" :rules="pathRules" label-position="top">
        <ElFormItem label="媒体库" prop="name">
          <ElSelect v-model="pathForm.name" class="library-page__control">
            <ElOption v-for="folder in folders" :key="folder.Id" :label="folder.Name" :value="folder.Name" />
          </ElSelect>
        </ElFormItem>
        <ElFormItem label="路径" prop="path">
          <ElInput v-model.trim="pathForm.path">
            <template #append>
              <ElButton @click="openDirectoryPicker('path')">
                <ElIcon>
                  <FolderOpened />
                </ElIcon>
              </ElButton>
            </template>
          </ElInput>
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="pathDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitPath">保存</ElButton>
      </template>
    </ElDialog>

    <DirectoryPicker
      v-model="directoryPickerVisible"
      title="选择媒体目录"
      @select="handleDirectorySelect"
    />
  </section>
</template>

<style scoped lang="scss">
.library-page {
  display: grid;
  align-content: start;
  gap: 16px;
  padding: 24px 32px 32px;
}

.library-page__heading {
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

.library-page__heading-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  justify-content: flex-end;
}

.library-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 12px;
}

.library-page__stat {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto;
  gap: 6px;
  align-items: baseline;
  min-height: 58px;
  padding: 12px 14px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: var(--admin-surface);

  span {
    min-width: 0;
    overflow: hidden;
    color: var(--admin-muted);
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    color: #0f766e;
    font-size: 24px;
    line-height: 1;
  }

  small {
    color: var(--admin-muted);
    font-size: 12px;
  }
}

.library-page__library-list {
  display: grid;
  align-content: start;
  gap: 12px;
  min-height: 280px;
}

.library-page__library-list :deep(.el-loading-mask) {
  border-radius: 8px;
}

.library-page__library-card {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 18px;
  align-items: start;
  padding: 16px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: var(--admin-surface);
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.04);
}

.library-page__library-main {
  display: flex;
  gap: 14px;
  min-width: 0;
}

.library-page__library-copy {
  display: grid;
  min-width: 0;
  gap: 12px;
  flex: 1;
}

.library-page__library-title {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  min-width: 0;

  div {
    display: grid;
    min-width: 0;
    gap: 4px;
  }

  h2 {
    margin: 0;
    overflow: hidden;
    color: var(--admin-text);
    font-size: 17px;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    overflow: hidden;
    color: var(--admin-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.library-page__paths {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  min-height: 34px;
}

.library-page__path-pill {
  display: inline-grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: 7px;
  align-items: center;
  max-width: min(100%, 460px);
  min-height: 32px;
  padding: 3px 3px 3px 9px;
  border: 1px solid #bfdbfe;
  border-radius: 8px;
  background: #f8fbff;
  color: #2563eb;
  font-size: 13px;

  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .el-button {
    width: 24px;
    height: 24px;
    min-height: 24px;
  }
}

.library-page__path-add {
  min-height: 32px;
}

.library-page__cover {
  position: relative;
  display: grid;
  width: 116px;
  height: 74px;
  flex: 0 0 116px;
  place-items: center;
  overflow: hidden;
  border: 1px solid rgba(15, 23, 42, 0.08);
  border-radius: 8px;
  background: linear-gradient(135deg, #e2e8f0, #f8fafc);
  color: #ffffff;
  font-size: 24px;
  font-weight: 700;
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.36);

  &::after {
    position: absolute;
    inset: 0;
    content: '';
    background:
      linear-gradient(135deg, rgba(255, 255, 255, 0.22), transparent 42%),
      linear-gradient(180deg, transparent 44%, rgba(15, 23, 42, 0.2));
    pointer-events: none;
  }

  span {
    position: relative;
    z-index: 1;
    text-shadow: 0 2px 6px rgba(15, 23, 42, 0.22);
  }

  img {
    position: absolute;
    inset: 0;
    display: none;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
}

.library-page__cover--movies {
  background: linear-gradient(135deg, #0f766e, #2563eb);
}

.library-page__cover--tvshows {
  background: linear-gradient(135deg, #7c3aed, #0e7490);
}

.library-page__cover--music {
  background: linear-gradient(135deg, #b45309, #be123c);
}

.library-page__cover--mixed {
  background: linear-gradient(135deg, #475569, #0f766e);
}

.library-page__card-actions {
  display: grid;
  grid-template-columns: repeat(2, max-content);
  gap: 8px;
  justify-content: end;
}

.library-page__card-actions :deep(.el-upload) {
  display: block;
}

:deep(.library-page__create-dialog) {
  overflow: hidden;
  border-radius: 8px;
}

:deep(.library-page__create-dialog .el-dialog__header) {
  margin: 0;
  padding: 18px 24px;
  border-bottom: 1px solid var(--admin-border);
}

:deep(.library-page__create-dialog .el-dialog__body) {
  padding: 20px 24px 4px;
}

:deep(.library-page__create-dialog .el-dialog__footer) {
  padding: 16px 24px 20px;
  border-top: 1px solid var(--admin-border);
}

.library-page__dialog-header {
  display: flex;
  align-items: center;
  gap: 12px;

  h2 {
    margin: 0;
    color: var(--admin-text);
    font-size: 20px;
    line-height: 1.25;
  }
}

.library-page__dialog-icon {
  display: grid;
  width: 36px;
  height: 36px;
  place-items: center;
  border-radius: 8px;
  color: #0f766e;
  background: #e6f4f1;
  font-size: 18px;
}

.library-page__create-form :deep(.el-form-item) {
  margin-bottom: 18px;
}

.library-page__create-form :deep(.el-form-item__label) {
  padding-bottom: 7px;
  color: var(--admin-text);
  font-weight: 700;
}

.library-page__type-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 10px;
  width: 100%;
}

.library-page__type-option {
  display: grid;
  gap: 6px;
  min-width: 0;
  min-height: 94px;
  padding: 12px 10px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  color: var(--admin-text);
  background: var(--admin-surface);
  cursor: pointer;
  text-align: left;
  transition:
    border-color 0.16s ease,
    background 0.16s ease,
    box-shadow 0.16s ease;

  .el-icon {
    display: grid;
    width: 30px;
    height: 30px;
    place-items: center;
    border-radius: 8px;
    color: #0f766e;
    background: #e6f4f1;
    font-size: 17px;
  }

  strong {
    overflow: hidden;
    font-size: 14px;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    overflow: hidden;
    color: var(--admin-muted);
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &:hover,
  &.is-active {
    border-color: rgba(15, 118, 110, 0.45);
    background: #f3fbf8;
    box-shadow: 0 10px 24px rgba(15, 118, 110, 0.08);
  }

  &.is-active {
    outline: 2px solid rgba(15, 118, 110, 0.12);
  }
}

.library-page__create-paths {
  display: grid;
  gap: 10px;
  width: 100%;
}

.library-page__create-path-input {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 104px;
  gap: 10px;
  align-items: stretch;

  .el-button {
    height: 100%;
    min-height: 72px;
  }
}

.library-page__create-path-list {
  display: grid;
  gap: 8px;
  max-height: 116px;
  overflow: auto;
}

.library-page__create-path-row {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: 8px;
  align-items: center;
  min-height: 34px;
  padding: 5px 5px 5px 10px;
  border: 1px solid #bfdbfe;
  border-radius: 8px;
  background: #f8fbff;
  color: #2563eb;
  font-size: 13px;

  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .el-button {
    width: 24px;
    height: 24px;
    min-height: 24px;
  }
}

.library-page__dialog-footer {
  display: flex;
  gap: 10px;
  justify-content: flex-end;
}

.library-page__control {
  width: 100%;
}

.library-page__path-input {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  width: 100%;
  align-items: start;
}

@media (max-width: 1120px) {
  .library-page__library-card {
    grid-template-columns: 1fr;
  }

  .library-page__card-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-start;
  }
}

@media (max-width: 760px) {
  .library-page {
    padding: 18px;
  }

  .library-page__heading {
    flex-direction: column;
  }

  .library-page__heading-actions {
    justify-content: flex-start;
  }

  .library-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .library-page__library-main {
    align-items: flex-start;
  }

  .library-page__cover {
    width: 96px;
    height: 64px;
    flex-basis: 96px;
  }

  .library-page__path-input {
    grid-template-columns: 1fr;
  }

  .library-page__type-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .library-page__create-path-input {
    grid-template-columns: 1fr;

    .el-button {
      min-height: 40px;
    }
  }
}

@media (max-width: 540px) {
  .library-page__stats {
    grid-template-columns: 1fr;
  }

  .library-page__library-main {
    display: grid;
  }

  .library-page__cover {
    width: 100%;
    height: 120px;
  }

  .library-page__library-title {
    display: grid;
  }

  .library-page__card-actions {
    display: grid;
    grid-template-columns: 1fr;
  }
}
</style>
