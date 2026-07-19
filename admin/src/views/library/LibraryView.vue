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
    await libraryApi.refreshLibrary(authStore.token);
    ElMessage.success('已触发媒体库扫描');
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
      <ElSpace wrap>
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
      </ElSpace>
    </div>

    <ElCard shadow="never">
      <ElTable v-loading="loading" :data="folders" empty-text="暂无媒体库">
        <ElTableColumn label="名称" min-width="180" prop="Name" />
        <ElTableColumn label="图片" width="104">
          <template #default="{ row }">
            <div class="library-page__cover">
              <span>{{ row.Name.slice(0, 1).toUpperCase() }}</span>
              <img
                :alt="row.Name"
                :src="folderImageUrl(row)"
                @error="hideFolderImage"
                @load="showFolderImage"
              />
            </div>
          </template>
        </ElTableColumn>
        <ElTableColumn label="类型" width="120" prop="CollectionType" />
        <ElTableColumn label="路径" min-width="320">
          <template #default="{ row }">
            <div class="library-page__paths">
              <ElTag v-for="path in row.Locations" :key="path" closable effect="plain" @close="removePath(row, path)">
                {{ path }}
              </ElTag>
              <ElButton link type="primary" @click="openPathDialog(row)">
                <ElIcon>
                  <Plus />
                </ElIcon>
                路径
              </ElButton>
            </div>
          </template>
        </ElTableColumn>
        <ElTableColumn align="right" label="操作" width="220">
          <template #default="{ row }">
            <ElUpload
              accept="image/jpeg,image/png,image/webp"
              :auto-upload="false"
              :on-change="folderImageChangeHandler(row)"
              :show-file-list="false"
            >
              <ElButton :loading="uploadingImageId === row.ItemId" link type="primary">上传图片</ElButton>
            </ElUpload>
            <ElButton :loading="deletingImageId === row.ItemId" link type="danger" @click="deleteFolderImage(row)">
              删除图片
            </ElButton>
            <ElButton link type="danger" @click="removeFolder(row)">删除</ElButton>
          </template>
        </ElTableColumn>
      </ElTable>
    </ElCard>

    <ElDialog v-model="createDialogVisible" title="新建媒体库" width="520px" @closed="resetForm">
      <ElForm ref="formRef" :model="form" :rules="rules" label-position="top">
        <ElFormItem label="名称" prop="name">
          <ElInput v-model.trim="form.name" />
        </ElFormItem>
        <ElFormItem label="媒体类型" prop="collectionType">
          <ElSelect v-model="form.collectionType" class="library-page__control">
            <ElOption v-for="item in collectionTypes" :key="item.value" :label="item.label" :value="item.value" />
          </ElSelect>
        </ElFormItem>
        <ElFormItem label="路径">
          <div class="library-page__path-input">
            <ElInput v-model="form.pathsText" :rows="4" type="textarea" />
            <ElButton @click="openDirectoryPicker('create')">
              <ElIcon>
                <FolderOpened />
              </ElIcon>
              选择
            </ElButton>
          </div>
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="createDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitCreate">保存</ElButton>
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
  gap: 18px;
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

.library-page__paths {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  min-height: 32px;
  align-items: center;
}

.library-page__cover {
  position: relative;
  display: grid;
  width: 64px;
  height: 40px;
  place-items: center;
  overflow: hidden;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #f1f5f9;
  color: #64748b;
  font-weight: 700;

  img {
    position: absolute;
    inset: 0;
    display: none;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
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

@media (max-width: 760px) {
  .library-page__heading {
    flex-direction: column;
  }

  .library-page__path-input {
    grid-template-columns: 1fr;
  }
}
</style>
