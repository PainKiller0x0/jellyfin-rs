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
const completedCount = computed(
  () => tasks.value.filter(task => resultText(task).toLowerCase().includes('complete')).length
);
const failedCount = computed(
  () => tasks.value.filter(task => resultText(task).toLowerCase().includes('fail')).length
);
const taskStats = computed(() => [
  {
    label: '任务总数',
    value: tasks.value.length,
    hint: '个'
  },
  {
    label: '运行中',
    value: runningCount.value,
    hint: '个'
  },
  {
    label: '已完成',
    value: completedCount.value,
    hint: '个'
  },
  {
    label: '失败',
    value: failedCount.value,
    hint: '个'
  }
]);

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

function stateType(state: string) {
  return state === 'Idle' ? 'info' : 'success';
}

function stateLabel(state: string) {
  if (state === 'Idle') {
    return '空闲';
  }
  if (state === 'Running') {
    return '运行中';
  }
  return state;
}

function resultText(task: ScheduledTask) {
  return task.LastExecutionResult?.Status ?? 'Never';
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

    <div class="tasks-page__stats">
      <div v-for="stat in taskStats" :key="stat.label" class="tasks-page__stat">
        <span>{{ stat.label }}</span>
        <strong>{{ stat.value }}</strong>
        <small>{{ stat.hint }}</small>
      </div>
    </div>

    <div v-loading="loading" class="tasks-page__task-list">
      <article v-for="task in tasks" :key="task.Id" class="tasks-page__task-card">
        <div class="tasks-page__task-main">
          <div class="tasks-page__task-icon">
            <ElIcon>
              <Clock />
            </ElIcon>
          </div>

          <div class="tasks-page__task-copy">
            <div class="tasks-page__task-title">
              <div>
                <h2>{{ task.Name }}</h2>
                <p>{{ task.Description || '暂无描述' }}</p>
              </div>
              <div class="tasks-page__tags">
                <ElTag :type="stateType(task.State)" effect="plain">{{ stateLabel(task.State) }}</ElTag>
                <ElTag :type="resultType(resultText(task))" effect="plain">{{ resultText(task) }}</ElTag>
              </div>
            </div>

            <div class="tasks-page__task-meta">
              <span>触发器：{{ triggerText(task) }}</span>
              <span>结束：{{ formatDate(task.LastExecutionResult?.EndTimeUtc) }}</span>
              <span>分类：{{ task.Category || task.Key || '-' }}</span>
            </div>
          </div>
        </div>

        <div class="tasks-page__card-actions">
          <ElButton :loading="actionTaskId === task.Id" type="primary" @click="runTask(task)">
            <ElIcon>
              <VideoPlay />
            </ElIcon>
            运行
          </ElButton>
          <ElButton :loading="actionTaskId === task.Id" type="warning" @click="stopTask(task)">
            <ElIcon>
              <SwitchButton />
            </ElIcon>
            停止
          </ElButton>
        </div>
      </article>

      <ElEmpty v-if="!loading && !tasks.length" :image-size="96" description="暂无计划任务" />
    </div>
  </section>
</template>

<style scoped lang="scss">
.tasks-page {
  display: grid;
  align-content: start;
  gap: 16px;
  padding: 24px 32px 32px;
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

.tasks-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 12px;
}

.tasks-page__stat {
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

.tasks-page__task-list {
  display: grid;
  align-content: start;
  gap: 12px;
  min-height: 260px;
}

.tasks-page__task-list :deep(.el-loading-mask) {
  border-radius: 8px;
}

.tasks-page__task-card {
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

.tasks-page__task-main {
  display: flex;
  gap: 14px;
  min-width: 0;
}

.tasks-page__task-icon {
  display: grid;
  width: 44px;
  height: 44px;
  flex: 0 0 44px;
  place-items: center;
  border-radius: 8px;
  color: #0f766e;
  background: #e6f4f1;
  font-size: 20px;
}

.tasks-page__task-copy {
  display: grid;
  min-width: 0;
  gap: 10px;
  flex: 1;
}

.tasks-page__task-title {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  min-width: 0;

  div {
    min-width: 0;
  }

  h2,
  p {
    margin: 0;
  }

  h2 {
    overflow: hidden;
    color: var(--admin-text);
    font-size: 17px;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  p {
    color: var(--admin-muted);
    font-size: 13px;
    line-height: 1.45;
  }
}

.tasks-page__tags {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  justify-content: flex-end;
}

.tasks-page__task-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;

  span {
    min-height: 30px;
    padding: 7px 10px;
    border: 1px solid var(--admin-border);
    border-radius: 8px;
    color: var(--admin-muted);
    background: var(--admin-surface-soft);
    font-size: 13px;
    line-height: 1.15;
  }
}

.tasks-page__card-actions {
  display: grid;
  grid-template-columns: repeat(2, max-content);
  gap: 8px;
  justify-content: end;
}

@media (max-width: 760px) {
  .tasks-page {
    padding: 18px;
  }

  .tasks-page__heading {
    flex-direction: column;
  }

  .tasks-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .tasks-page__task-card {
    grid-template-columns: 1fr;
  }

  .tasks-page__task-title {
    display: grid;
  }

  .tasks-page__tags,
  .tasks-page__card-actions {
    justify-content: flex-start;
  }
}

@media (max-width: 540px) {
  .tasks-page__stats,
  .tasks-page__card-actions {
    grid-template-columns: 1fr;
  }

  .tasks-page__task-main {
    display: grid;
  }

  .tasks-page__task-meta {
    display: grid;
  }
}
</style>
