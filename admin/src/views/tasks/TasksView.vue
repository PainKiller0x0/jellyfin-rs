<script setup lang="ts">
import dayjs from 'dayjs';
import { ElMessage } from 'element-plus';
import { computed, onMounted, ref } from 'vue';

import * as serverApi from '@/services/server';
import { useAuthStore } from '@/stores/auth';
import type { ScheduledTask } from '@/types/server';

const authStore = useAuthStore();
const loading = ref(false);
const actionTaskId = ref('');
const tasks = ref<ScheduledTask[]>([]);

const runningCount = computed(() => tasks.value.filter(task => task.State !== 'Idle').length);

async function loadTasks() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    tasks.value = await serverApi.scheduledTasks(authStore.token);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载计划任务失败');
  } finally {
    loading.value = false;
  }
}

async function runTask(task: ScheduledTask) {
  if (!authStore.token) {
    return;
  }

  actionTaskId.value = task.Id;
  try {
    await serverApi.runScheduledTask(authStore.token, task.Id);
    ElMessage.success('任务已启动');
    await loadTasks();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '启动任务失败');
  } finally {
    actionTaskId.value = '';
  }
}

async function stopTask(task: ScheduledTask) {
  if (!authStore.token) {
    return;
  }

  actionTaskId.value = task.Id;
  try {
    await serverApi.stopScheduledTask(authStore.token, task.Id);
    ElMessage.success('任务已停止');
    await loadTasks();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '停止任务失败');
  } finally {
    actionTaskId.value = '';
  }
}

function formatDate(value?: string | null) {
  if (!value) {
    return '-';
  }

  const date = dayjs(value);
  return date.isValid() ? date.format('YYYY-MM-DD HH:mm:ss') : value;
}

function resultType(status?: string) {
  const value = status?.toLowerCase() ?? '';
  if (value.includes('fail')) {
    return 'danger';
  }
  if (value.includes('cancel')) {
    return 'warning';
  }
  if (value.includes('complete')) {
    return 'success';
  }
  return 'info';
}

function triggerText(task: ScheduledTask) {
  const triggers = task.Triggers ?? [];
  if (!triggers.length) {
    return '-';
  }

  return triggers.map(trigger => trigger.Type).join(', ');
}

onMounted(loadTasks);
</script>

<template>
  <section class="admin-page tasks-page">
    <div class="tasks-page__heading">
      <div>
        <h1>计划任务</h1>
        <p>{{ tasks.length }} 个任务，{{ runningCount }} 个运行中。</p>
      </div>
      <ElButton :loading="loading" type="primary" @click="loadTasks">
        <ElIcon>
          <Refresh />
        </ElIcon>
        刷新
      </ElButton>
    </div>

    <ElCard shadow="never">
      <ElTable v-loading="loading" :data="tasks" empty-text="暂无计划任务">
        <ElTableColumn label="任务" min-width="220">
          <template #default="{ row }">
            <div class="tasks-page__task">
              <strong>{{ row.Name }}</strong>
              <span>{{ row.Description }}</span>
            </div>
          </template>
        </ElTableColumn>
        <ElTableColumn label="状态" width="110">
          <template #default="{ row }">
            <ElTag :type="row.State === 'Idle' ? 'info' : 'success'" effect="plain">{{ row.State }}</ElTag>
          </template>
        </ElTableColumn>
        <ElTableColumn label="触发器" min-width="150">
          <template #default="{ row }">
            {{ triggerText(row) }}
          </template>
        </ElTableColumn>
        <ElTableColumn label="最近结果" width="130">
          <template #default="{ row }">
            <ElTag :type="resultType(row.LastExecutionResult?.Status)" effect="plain">
              {{ row.LastExecutionResult?.Status ?? 'Never' }}
            </ElTag>
          </template>
        </ElTableColumn>
        <ElTableColumn label="结束时间" min-width="170">
          <template #default="{ row }">
            {{ formatDate(row.LastExecutionResult?.EndTimeUtc) }}
          </template>
        </ElTableColumn>
        <ElTableColumn align="right" label="操作" width="180">
          <template #default="{ row }">
            <ElButton :loading="actionTaskId === row.Id" link type="primary" @click="runTask(row)">运行</ElButton>
            <ElButton :loading="actionTaskId === row.Id" link type="warning" @click="stopTask(row)">停止</ElButton>
          </template>
        </ElTableColumn>
      </ElTable>
    </ElCard>
  </section>
</template>

<style scoped lang="scss">
.tasks-page {
  display: grid;
  gap: 18px;
}

.tasks-page__heading {
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

.tasks-page__task {
  display: grid;
  gap: 4px;

  strong {
    font-size: 14px;
  }

  span {
    color: var(--admin-muted);
    font-size: 12px;
    line-height: 1.45;
  }
}

@media (max-width: 760px) {
  .tasks-page__heading {
    flex-direction: column;
  }
}
</style>
