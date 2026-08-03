<script setup lang="ts">
import { Folder, Select } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { computed, ref, watch } from 'vue';

import * as libraryApi from '@/services/library';
import { useAuthStore } from '@/stores/auth';
import type { DirectoryEntry } from '@/types/library';

const props = withDefaults(
  defineProps<{
    modelValue: boolean;
    title?: string;
  }>(),
  {
    title: '选择目录'
  }
);

const emit = defineEmits<{
  'update:modelValue': [value: boolean];
  select: [path: string];
}>();

const authStore = useAuthStore();
const loading = ref(false);
const currentPath = ref('');
const pathInput = ref('');
const entries = ref<DirectoryEntry[]>([]);

const visible = computed({
  get: () => props.modelValue,
  set: value => emit('update:modelValue', value)
});

async function loadRoots() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    currentPath.value = '';
    pathInput.value = '';
    entries.value = await libraryApi.drives(authStore.token);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载根目录失败');
  } finally {
    loading.value = false;
  }
}

async function loadDirectory(path: string) {
  if (!authStore.token || !path.trim()) {
    return;
  }

  loading.value = true;
  try {
    const normalizedPath = path.trim();
    entries.value = await libraryApi.directoryContents(authStore.token, normalizedPath);
    currentPath.value = normalizedPath;
    pathInput.value = normalizedPath;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载目录失败');
  } finally {
    loading.value = false;
  }
}

async function loadDefaultDirectory() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    const result = await libraryApi.defaultDirectoryBrowser(authStore.token);
    if (result.Path) {
      await loadDirectory(result.Path);
    } else {
      await loadRoots();
    }
  } catch {
    await loadRoots();
  } finally {
    loading.value = false;
  }
}

async function goParent() {
  if (!authStore.token || !currentPath.value) {
    await loadRoots();
    return;
  }

  try {
    const parent = await libraryApi.parentPath(authStore.token, currentPath.value);
    if (parent) {
      await loadDirectory(parent);
    } else {
      await loadRoots();
    }
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '无法进入上一级');
  }
}

async function choose(path: string) {
  if (!authStore.token || !path.trim()) {
    return;
  }

  try {
    await libraryApi.validateDirectoryPath(authStore.token, path);
    emit('select', path);
    visible.value = false;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '目录不可用');
  }
}

function openEntry(row: DirectoryEntry) {
  loadDirectory(row.Path);
}

watch(
  () => props.modelValue,
  open => {
    if (open) {
      loadDefaultDirectory();
    }
  }
);
</script>

<template>
  <ElDialog v-model="visible" :title="title" width="760px">
    <div class="directory-picker">
      <div class="directory-picker__toolbar">
        <ElInput v-model="pathInput" clearable placeholder="输入路径后打开">
          <template #prepend>路径</template>
        </ElInput>
        <ElButton @click="loadDirectory(pathInput)">打开</ElButton>
        <ElButton @click="goParent">上一级</ElButton>
        <ElButton @click="loadRoots">根目录</ElButton>
      </div>

      <div class="directory-picker__current">
        <span>{{ currentPath || '根目录' }}</span>
        <ElButton :disabled="!currentPath" type="primary" @click="choose(currentPath)">
          <ElIcon>
            <Select />
          </ElIcon>
          选择当前目录
        </ElButton>
      </div>

      <ElTable
        v-loading="loading"
        :data="entries"
        empty-text="暂无可选目录"
        height="360"
        @row-dblclick="openEntry"
      >
        <ElTableColumn label="目录" min-width="220">
          <template #default="{ row }">
            <div class="directory-picker__name">
              <ElIcon>
                <Folder />
              </ElIcon>
              <span>{{ row.Name || row.Path }}</span>
            </div>
          </template>
        </ElTableColumn>
        <ElTableColumn label="路径" min-width="320" prop="Path" />
        <ElTableColumn align="right" label="操作" width="150">
          <template #default="{ row }">
            <ElButton link type="primary" @click="loadDirectory(row.Path)">进入</ElButton>
            <ElButton link type="success" @click="choose(row.Path)">选择</ElButton>
          </template>
        </ElTableColumn>
      </ElTable>
    </div>
  </ElDialog>
</template>

<style scoped lang="scss">
.directory-picker {
  display: grid;
  gap: 14px;
}

.directory-picker__toolbar {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto auto;
  gap: 8px;
}

.directory-picker__current {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: var(--admin-surface-soft);

  span {
    overflow: hidden;
    color: var(--admin-muted);
    font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace;
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.directory-picker__name {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;

  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

@media (max-width: 760px) {
  .directory-picker__toolbar {
    grid-template-columns: 1fr;
  }

  .directory-picker__current {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
