<script setup lang="ts">
import dayjs from 'dayjs';
import { computed, onMounted, reactive, ref } from 'vue';

import * as serverApi from '@/services/server';
import { useAuthStore } from '@/stores/auth';
import type { ActivityLogEntry, ItemCounts, PlaybackSession, ScheduledTask, SystemInfo } from '@/types/server';

const authStore = useAuthStore();
const loading = ref(false);
const loadError = ref('');

const state = reactive<{
  system: SystemInfo | null;
  counts: ItemCounts | null;
  sessions: PlaybackSession[];
  tasks: ScheduledTask[];
  activities: ActivityLogEntry[];
}>({
  system: null,
  counts: null,
  sessions: [],
  tasks: [],
  activities: []
});

const cards = computed(() => [
  {
    label: '服务状态',
    value: state.system ? '在线' : '-',
    hint: state.system?.Version ? `v${state.system.Version}` : '等待服务器响应',
    tone: 'success'
  },
  {
    label: '媒体项目',
    value: formatNumber(state.counts?.ItemCount),
    hint: `${formatNumber(state.counts?.MovieCount)} 部影片 / ${formatNumber(state.counts?.SeriesCount)} 部剧集`,
    tone: 'primary'
  },
  {
    label: '活跃会话',
    value: formatNumber(state.sessions.length),
    hint: state.sessions.length ? '当前有客户端在线' : '暂无客户端会话',
    tone: 'warning'
  },
  {
    label: '计划任务',
    value: formatNumber(state.tasks.length),
    hint: taskSummary.value,
    tone: 'info'
  }
]);

const taskSummary = computed(() => {
  const running = state.tasks.filter(task => task.State !== 'Idle').length;
  return running ? `${running} 个任务运行中` : '任务空闲';
});

async function loadDashboard() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  loadError.value = '';
  try {
    const [system, counts, sessions, tasks, activities] = await Promise.all([
      serverApi.systemInfo(authStore.token),
      serverApi.itemCounts(authStore.token),
      serverApi.activeSessions(authStore.token),
      serverApi.scheduledTasks(authStore.token),
      serverApi.activityLog(authStore.token)
    ]);
    state.system = system;
    state.counts = counts;
    state.sessions = sessions;
    state.tasks = tasks;
    state.activities = activities.Items;
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : '加载控制台失败';
  } finally {
    loading.value = false;
  }
}

function formatNumber(value: number | undefined) {
  return typeof value === 'number' ? value.toLocaleString() : '-';
}

function formatDate(value?: string | null) {
  if (!value) {
    return '-';
  }

  const date = dayjs(value);
  return date.isValid() ? date.format('YYYY-MM-DD HH:mm') : value;
}

function severityType(severity: string) {
  const value = severity.toLowerCase();
  if (value.includes('error') || value.includes('fatal')) {
    return 'danger';
  }
  if (value.includes('warn')) {
    return 'warning';
  }
  return 'info';
}

onMounted(loadDashboard);
</script>

<template>
  <section class="admin-page dashboard-page">
    <div class="dashboard-page__heading">
      <div>
        <h1>控制台</h1>
        <p>管理媒体库、用户、任务和服务器配置。</p>
      </div>
      <ElButton :loading="loading" type="primary" @click="loadDashboard">
        <ElIcon>
          <Refresh />
        </ElIcon>
        刷新
      </ElButton>
    </div>

    <ElAlert v-if="loadError" :closable="false" :title="loadError" type="error" />

    <div class="dashboard-page__stats">
      <ElCard v-for="card in cards" :key="card.label" class="dashboard-page__stat" shadow="never">
        <div class="dashboard-page__stat-label">{{ card.label }}</div>
        <div class="dashboard-page__stat-value">{{ card.value }}</div>
        <ElTag :type="card.tone" effect="plain">{{ card.hint }}</ElTag>
      </ElCard>
    </div>

    <div class="dashboard-page__grid">
      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <Monitor />
            </ElIcon>
            <span>服务器</span>
          </div>
        </template>

        <ElDescriptions :column="1" border>
          <ElDescriptionsItem label="名称">{{ state.system?.ServerName ?? '-' }}</ElDescriptionsItem>
          <ElDescriptionsItem label="版本">{{ state.system?.Version ?? '-' }}</ElDescriptionsItem>
          <ElDescriptionsItem label="系统">{{ state.system?.OperatingSystem ?? '-' }}</ElDescriptionsItem>
          <ElDescriptionsItem label="本地地址">{{ state.system?.LocalAddress ?? '-' }}</ElDescriptionsItem>
        </ElDescriptions>
      </ElCard>

      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <Clock />
            </ElIcon>
            <span>计划任务</span>
          </div>
        </template>

        <ElTable :data="state.tasks" empty-text="暂无计划任务" height="260">
          <ElTableColumn label="任务" min-width="160" prop="Name" />
          <ElTableColumn label="状态" width="100">
            <template #default="{ row }">
              <ElTag :type="row.State === 'Idle' ? 'info' : 'success'" effect="plain">{{ row.State }}</ElTag>
            </template>
          </ElTableColumn>
          <ElTableColumn label="上次执行" min-width="150">
            <template #default="{ row }">
              {{ formatDate(row.LastExecutionResult?.EndTimeUtc) }}
            </template>
          </ElTableColumn>
        </ElTable>
      </ElCard>
    </div>

    <div class="dashboard-page__grid">
      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <Connection />
            </ElIcon>
            <span>活跃会话</span>
          </div>
        </template>

        <ElTable :data="state.sessions" empty-text="暂无活跃会话" height="260">
          <ElTableColumn label="客户端" min-width="150" prop="Client" />
          <ElTableColumn label="设备" min-width="150" prop="DeviceName" />
          <ElTableColumn label="播放内容" min-width="160">
            <template #default="{ row }">
              {{ row.NowPlayingItemName || '-' }}
            </template>
          </ElTableColumn>
          <ElTableColumn label="活跃时间" min-width="150">
            <template #default="{ row }">
              {{ formatDate(row.LastActivityDate) }}
            </template>
          </ElTableColumn>
        </ElTable>
      </ElCard>

      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <Tickets />
            </ElIcon>
            <span>活动日志</span>
          </div>
        </template>

        <ElTimeline class="dashboard-page__timeline">
          <ElTimelineItem
            v-for="item in state.activities"
            :key="`${item.Date}-${item.Name}`"
            :timestamp="formatDate(item.Date)"
            :type="severityType(item.Severity)"
          >
            <div class="dashboard-page__activity">
              <strong>{{ item.Name }}</strong>
              <span>{{ item.Type }}</span>
            </div>
          </ElTimelineItem>
          <ElEmpty v-if="!state.activities.length" :image-size="80" description="暂无活动日志" />
        </ElTimeline>
      </ElCard>
    </div>
  </section>
</template>

<style scoped lang="scss">
.dashboard-page {
  display: grid;
  gap: 18px;
}

.dashboard-page__heading {
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

.dashboard-page__stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;
}

.dashboard-page__stat {
  min-height: 132px;
}

.dashboard-page__stat-label {
  color: var(--admin-muted);
  font-size: 13px;
}

.dashboard-page__stat-value {
  margin: 12px 0 16px;
  font-size: 28px;
  font-weight: 800;
}

.dashboard-page__panel-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 700;
}

.dashboard-page__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.dashboard-page__timeline {
  height: 260px;
  margin: 0;
  overflow: auto;
}

.dashboard-page__activity {
  display: grid;
  gap: 4px;

  strong {
    font-size: 14px;
  }

  span {
    color: var(--admin-muted);
    font-size: 12px;
  }
}

@media (max-width: 980px) {
  .dashboard-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .dashboard-page__grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 640px) {
  .dashboard-page__heading {
    flex-direction: column;
  }

  .dashboard-page__stats {
    grid-template-columns: 1fr;
  }
}
</style>
