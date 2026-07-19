<script setup lang="ts">
import dayjs from 'dayjs';
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from 'vue';

import * as serverApi from '@/services/server';
import { useAuthStore } from '@/stores/auth';
import type {
  AdminHttpLogEntry,
  ActivityLogEntry,
  ItemCounts,
  PlaybackMap,
  PlaybackRegion,
  PlaybackSession,
  ScheduledTask,
  SystemInfo
} from '@/types/server';

const authStore = useAuthStore();
const loading = ref(false);
const loadError = ref('');
const logPanelRef = ref<HTMLElement>();
const logLastId = ref(0);
let logPollTimer: number | undefined;
let playbackPollTimer: number | undefined;

const state = reactive<{
  system: SystemInfo | null;
  counts: ItemCounts | null;
  playbackMap: PlaybackMap | null;
  sessions: PlaybackSession[];
  tasks: ScheduledTask[];
  activities: ActivityLogEntry[];
  httpLogs: AdminHttpLogEntry[];
}>({
  system: null,
  counts: null,
  playbackMap: null,
  sessions: [],
  tasks: [],
  activities: [],
  httpLogs: []
});

const regionRows = computed(() => state.playbackMap?.Regions ?? []);
const recentPlaybackEvents = computed(() => state.playbackMap?.RecentEvents.slice(0, 6) ?? []);
const maxRegionWeight = computed(() =>
  Math.max(1, ...regionRows.value.map(region => Math.max(region.UserCount, region.PlayCount)))
);

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
    label: '播放次数',
    value: formatNumber(state.playbackMap?.TotalPlayCount),
    hint: `${formatNumber(state.playbackMap?.RegionCount)} 个地区/IP 段`,
    tone: 'success'
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
    const [system, counts, sessions, tasks, activities, playbackMap, logs] = await Promise.all([
      serverApi.systemInfo(authStore.token),
      serverApi.itemCounts(authStore.token),
      serverApi.activeSessions(authStore.token),
      serverApi.scheduledTasks(authStore.token),
      serverApi.activityLog(authStore.token),
      serverApi.playbackMap(authStore.token),
      serverApi.adminLogs(authStore.token, 0, 120)
    ]);
    state.system = system;
    state.counts = counts;
    state.sessions = sessions;
    state.tasks = tasks;
    state.activities = activities.Items;
    state.playbackMap = playbackMap;
    state.httpLogs = logs.Items;
    logLastId.value = logs.LastId;
    await nextTick(scrollLogsToBottom);
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : '加载控制台失败';
  } finally {
    loading.value = false;
  }
}

async function pollLogs() {
  if (!authStore.token) {
    return;
  }
  try {
    const result = await serverApi.adminLogs(authStore.token, logLastId.value, 120);
    if (!result.Items.length) {
      return;
    }
    state.httpLogs.push(...result.Items);
    state.httpLogs = state.httpLogs.slice(-220);
    logLastId.value = result.LastId;
    await nextTick(scrollLogsToBottom);
  } catch {
    // Keep the dashboard readable if a transient poll fails.
  }
}

async function pollPlaybackMap() {
  if (!authStore.token) {
    return;
  }
  try {
    state.playbackMap = await serverApi.playbackMap(authStore.token);
  } catch {
    // Transient polling errors are ignored; manual refresh still reports failures.
  }
}

function startPolling() {
  window.clearInterval(logPollTimer);
  window.clearInterval(playbackPollTimer);
  logPollTimer = window.setInterval(pollLogs, 2_000);
  playbackPollTimer = window.setInterval(pollPlaybackMap, 5_000);
}

function stopPolling() {
  window.clearInterval(logPollTimer);
  window.clearInterval(playbackPollTimer);
}

function scrollLogsToBottom() {
  if (logPanelRef.value) {
    logPanelRef.value.scrollTop = logPanelRef.value.scrollHeight;
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

function statusType(statusCode: number) {
  if (statusCode >= 500) {
    return 'danger';
  }
  if (statusCode >= 400) {
    return 'warning';
  }
  return 'success';
}

function markerStyle(region: PlaybackRegion) {
  const ratio = Math.min(1, Math.max(region.UserCount, region.PlayCount) / maxRegionWeight.value);
  const size = 18 + ratio * 28;
  const alpha = 0.28 + ratio * 0.62;
  return {
    left: `${region.X}%`,
    top: `${region.Y}%`,
    width: `${size}px`,
    height: `${size}px`,
    backgroundColor: `rgba(22, 163, 74, ${alpha})`,
    borderColor: `rgba(21, 128, 61, ${Math.min(1, alpha + 0.18)})`
  };
}

function logLine(entry: AdminHttpLogEntry) {
  const query = entry.Query ? `?${entry.Query}` : '';
  return `${entry.Method} ${entry.Path}${query}`;
}

onMounted(async () => {
  await loadDashboard();
  startPolling();
});

onBeforeUnmount(stopPolling);
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

    <div class="dashboard-page__grid dashboard-page__grid--map">
      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <Location />
            </ElIcon>
            <span>用户分布 IP 地图</span>
          </div>
        </template>

        <div class="dashboard-page__map">
          <div class="dashboard-page__map-surface">
            <button
              v-for="region in regionRows"
              :key="region.RegionCode"
              class="dashboard-page__map-marker"
              :style="markerStyle(region)"
              type="button"
            >
              <span>{{ region.UserCount }}</span>
            </button>
            <ElEmpty v-if="!regionRows.length" :image-size="80" description="暂无播放分布" />
          </div>
          <div class="dashboard-page__region-list">
            <div v-for="region in regionRows.slice(0, 8)" :key="region.RegionCode" class="dashboard-page__region">
              <div>
                <strong>{{ region.Region }}</strong>
                <span>{{ region.SampleIps.join(', ') || '-' }}</span>
              </div>
              <ElTag type="success" effect="plain">{{ region.PlayCount }} 次 / {{ region.UserCount }} 用户</ElTag>
            </div>
          </div>
        </div>
      </ElCard>

      <ElCard class="dashboard-page__panel" shadow="never">
        <template #header>
          <div class="dashboard-page__panel-title">
            <ElIcon>
              <VideoCamera />
            </ElIcon>
            <span>最近播放</span>
          </div>
        </template>

        <ElTimeline class="dashboard-page__timeline">
          <ElTimelineItem
            v-for="event in recentPlaybackEvents"
            :key="`${event.UnixTime}-${event.UserId}-${event.ItemId}`"
            :timestamp="formatDate(event.Date)"
            type="success"
          >
            <div class="dashboard-page__activity">
              <strong>{{ event.ItemName || event.ItemId }}</strong>
              <span>{{ event.Region }} · {{ event.Ip }} · {{ event.Client || '-' }}</span>
            </div>
          </ElTimelineItem>
          <ElEmpty v-if="!recentPlaybackEvents.length" :image-size="80" description="暂无播放记录" />
        </ElTimeline>
      </ElCard>
    </div>

    <ElCard class="dashboard-page__panel" shadow="never">
      <template #header>
        <div class="dashboard-page__panel-title">
          <ElIcon>
            <Document />
          </ElIcon>
          <span>实时访问日志</span>
        </div>
      </template>

      <div ref="logPanelRef" class="dashboard-page__log-stream">
        <div v-for="entry in state.httpLogs" :key="entry.Id" class="dashboard-page__log-row">
          <span class="dashboard-page__log-time">{{ formatDate(entry.Date) }}</span>
          <ElTag :type="statusType(entry.StatusCode)" effect="dark" size="small">{{ entry.StatusCode }}</ElTag>
          <strong>{{ logLine(entry) }}</strong>
          <span>{{ entry.RemoteAddress || '-' }}</span>
          <span>{{ entry.Client || entry.UserAgent || '-' }}</span>
          <span>{{ entry.ElapsedMs }}ms</span>
        </div>
        <ElEmpty v-if="!state.httpLogs.length" :image-size="80" description="暂无访问日志" />
      </div>
    </ElCard>

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
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 16px;
}

.dashboard-page__stat {
  min-height: 132px;

  :deep(.el-card__body) {
    display: grid;
    min-height: 96px;
  }
}

.dashboard-page__stat-label {
  color: var(--admin-muted);
  font-size: 13px;
}

.dashboard-page__stat-value {
  margin: 12px 0 16px;
  font-size: 28px;
  font-weight: 800;
  line-height: 1.1;
}

.dashboard-page__panel-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 700;
}

.dashboard-page__panel {
  min-width: 0;
}

.dashboard-page__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.dashboard-page__grid--map {
  grid-template-columns: minmax(0, 1.35fr) minmax(320px, 0.65fr);
  align-items: stretch;
}

.dashboard-page__map {
  display: grid;
  gap: 14px;
}

.dashboard-page__map-surface {
  position: relative;
  display: grid;
  min-height: 300px;
  place-items: center;
  overflow: hidden;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background:
    radial-gradient(ellipse at 18% 36%, rgba(96, 165, 250, 0.28) 0 6%, transparent 6.4%),
    radial-gradient(ellipse at 28% 54%, rgba(96, 165, 250, 0.25) 0 9%, transparent 9.5%),
    radial-gradient(ellipse at 48% 42%, rgba(96, 165, 250, 0.22) 0 13%, transparent 13.5%),
    radial-gradient(ellipse at 66% 48%, rgba(96, 165, 250, 0.24) 0 14%, transparent 14.5%),
    radial-gradient(ellipse at 78% 67%, rgba(96, 165, 250, 0.22) 0 7%, transparent 7.6%),
    linear-gradient(180deg, #f8fafc 0%, #eef6f2 100%);
}

.dashboard-page__map-surface::before {
  position: absolute;
  inset: 18px;
  content: '';
  border: 1px solid rgba(148, 163, 184, 0.28);
  border-radius: 8px;
  background-image:
    linear-gradient(rgba(148, 163, 184, 0.18) 1px, transparent 1px),
    linear-gradient(90deg, rgba(148, 163, 184, 0.18) 1px, transparent 1px);
  background-size: 64px 64px;
  pointer-events: none;
}

.dashboard-page__map-marker {
  position: absolute;
  z-index: 1;
  display: grid;
  min-width: 18px;
  min-height: 18px;
  padding: 0;
  place-items: center;
  border: 2px solid;
  border-radius: 999px;
  color: #052e16;
  cursor: default;
  font-size: 12px;
  font-weight: 800;
  line-height: 1;
  transform: translate(-50%, -50%);
  box-shadow: 0 8px 20px rgba(22, 101, 52, 0.18);
}

.dashboard-page__map-marker span {
  display: grid;
  width: 100%;
  height: 100%;
  min-width: 16px;
  place-items: center;
}

.dashboard-page__region-list {
  display: grid;
  gap: 10px;
}

.dashboard-page__region {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px;
  align-items: center;
  padding: 10px 12px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
}

.dashboard-page__region div {
  display: grid;
  min-width: 0;
  gap: 4px;
}

.dashboard-page__region strong {
  overflow: hidden;
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dashboard-page__region span {
  overflow: hidden;
  color: var(--admin-muted);
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dashboard-page__log-stream {
  display: grid;
  max-height: 360px;
  overflow: auto;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #0f172a;
}

.dashboard-page__log-row {
  display: grid;
  grid-template-columns: 132px 64px minmax(220px, 1fr) minmax(120px, 0.5fr) minmax(120px, 0.5fr) 72px;
  gap: 10px;
  align-items: center;
  min-height: 38px;
  padding: 8px 12px;
  border-bottom: 1px solid rgba(148, 163, 184, 0.18);
  color: #cbd5e1;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
  font-size: 12px;
  line-height: 1.35;
}

.dashboard-page__log-row:last-child {
  border-bottom: 0;
}

.dashboard-page__log-row strong,
.dashboard-page__log-row span {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dashboard-page__log-row strong {
  color: #f8fafc;
  font-weight: 700;
}

.dashboard-page__log-time {
  color: #93c5fd;
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
  .dashboard-page__grid {
    grid-template-columns: 1fr;
  }

  .dashboard-page__grid--map {
    grid-template-columns: 1fr;
  }

  .dashboard-page__log-row {
    grid-template-columns: 120px 58px minmax(180px, 1fr);
  }

  .dashboard-page__log-row span:nth-last-child(-n + 3) {
    display: none;
  }
}

@media (max-width: 640px) {
  .dashboard-page__heading {
    flex-direction: column;
  }

  .dashboard-page__map-surface {
    min-height: 240px;
  }

  .dashboard-page__region {
    grid-template-columns: 1fr;
  }

  .dashboard-page__log-stream {
    max-height: 320px;
  }

  .dashboard-page__log-row {
    grid-template-columns: 1fr 58px;
  }

  .dashboard-page__log-row strong {
    grid-column: 1 / -1;
    order: 3;
  }

  .dashboard-page__log-time {
    min-width: 0;
  }
}
</style>
