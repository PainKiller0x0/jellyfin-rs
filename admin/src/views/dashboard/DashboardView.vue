<script setup lang="ts">
import { geoMercator, geoPath } from 'd3-geo';
import dayjs from 'dayjs';
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import chinaMapSource from 'china-map-geojson/lib/china';
import type { Feature, Geometry, GeoJsonProperties } from 'geojson';

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

const CHINA_MAP_WIDTH = 1000;
const CHINA_MAP_HEIGHT = 500;
type ChinaProvinceProperties = GeoJsonProperties & {
  id?: string;
  name?: string;
};

const chinaFeatures = chinaMapSource.features as Feature<Geometry, ChinaProvinceProperties>[];
const chinaFeatureCollection = chinaMapSource;
const chinaProjection = geoMercator().fitExtent(
  [
    [74, 22],
    [CHINA_MAP_WIDTH - 58, CHINA_MAP_HEIGHT - 22]
  ],
  chinaFeatureCollection
);
const chinaPath = geoPath(chinaProjection);
const chinaProvincePaths = chinaFeatures.map(feature => ({
  id: feature.properties?.id ?? feature.properties?.name,
  name: feature.properties?.name ?? '',
  d: chinaPath(feature) ?? ''
}));

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
  const size = 16 + ratio * 10;
  const alpha = 0.7 + ratio * 0.2;
  const x = Math.min(94, Math.max(6, region.X));
  const y = Math.min(90, Math.max(8, region.Y));
  return {
    left: `${x}%`,
    top: `${y}%`,
    width: `${size}px`,
    height: `${size}px`,
    '--marker-alpha': alpha.toString()
  };
}

function regionTooltip(region: PlaybackRegion) {
  const network = region.IsPrivate ? '内网' : '公网';
  return `${region.Region} · ${network} · ${region.PlayCount} 次播放 · ${region.UserCount} 用户`;
}

function markerLabel(region: PlaybackRegion) {
  return region.UserCount > 99 ? '99+' : region.UserCount.toString();
}

function regionBarStyle(region: PlaybackRegion) {
  const ratio = Math.min(1, region.PlayCount / maxRegionWeight.value);
  return {
    width: `${Math.max(8, ratio * 100)}%`
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
            <svg
              class="dashboard-page__map-canvas"
              :viewBox="`0 0 ${CHINA_MAP_WIDTH} ${CHINA_MAP_HEIGHT}`"
              preserveAspectRatio="xMidYMid meet"
              aria-hidden="true"
              focusable="false"
            >
              <defs>
                <linearGradient id="mapOcean" x1="0" x2="1" y1="0" y2="1">
                  <stop offset="0%" stop-color="#f8fbff" />
                  <stop offset="100%" stop-color="#edf6f3" />
                </linearGradient>
                <linearGradient id="chinaMapLand" x1="0" x2="1" y1="0" y2="1">
                  <stop offset="0%" stop-color="#d8ece4" />
                  <stop offset="100%" stop-color="#b9d7cc" />
                </linearGradient>
                <filter id="mapLandShadow" x="-4%" y="-8%" width="108%" height="116%">
                  <feDropShadow dx="0" dy="4" flood-color="#0f172a" flood-opacity="0.08" stdDeviation="5" />
                </filter>
              </defs>
              <rect class="dashboard-page__map-ocean" :width="CHINA_MAP_WIDTH" :height="CHINA_MAP_HEIGHT" rx="18" />
              <g class="dashboard-page__map-grid">
                <path d="M110 98H900M110 182H900M110 266H900M110 350H900M110 434H900" />
                <path d="M188 62V455M326 62V455M464 62V455M602 62V455M740 62V455M878 62V455" />
              </g>
              <g class="dashboard-page__china-map" filter="url(#mapLandShadow)">
                <path
                  v-for="province in chinaProvincePaths"
                  :key="province.id"
                  class="dashboard-page__china-province"
                  :d="province.d"
                />
              </g>
            </svg>
            <div class="dashboard-page__map-badge">
              <span>{{ formatNumber(state.playbackMap?.RegionCount) }}</span>
              <small>地区/IP 段</small>
            </div>
            <ElTooltip
              v-for="region in regionRows"
              :key="region.RegionCode"
              :content="regionTooltip(region)"
              placement="top"
              :show-after="120"
            >
              <button
                class="dashboard-page__map-marker"
                :class="{ 'dashboard-page__map-marker--private': region.IsPrivate }"
                :style="markerStyle(region)"
                :aria-label="regionTooltip(region)"
                type="button"
              >
                <span>{{ markerLabel(region) }}</span>
              </button>
            </ElTooltip>
            <div class="dashboard-page__map-legend" aria-hidden="true">
              <span><i class="dashboard-page__map-dot"></i>公网</span>
              <span><i class="dashboard-page__map-dot dashboard-page__map-dot--private"></i>内网</span>
            </div>
            <div v-if="!regionRows.length" class="dashboard-page__map-empty">暂无播放分布</div>
          </div>
          <div class="dashboard-page__region-list">
            <div v-for="region in regionRows.slice(0, 8)" :key="region.RegionCode" class="dashboard-page__region">
              <div>
                <strong>{{ region.Region }}</strong>
                <span>{{ region.SampleIps.join(', ') || '-' }}</span>
                <b :style="regionBarStyle(region)"></b>
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
  height: clamp(320px, 31vw, 430px);
  min-height: 320px;
  overflow: hidden;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #f8fbff;
  isolation: isolate;
}

.dashboard-page__map-canvas {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
}

.dashboard-page__map-ocean {
  fill: url('#mapOcean');
  stroke: rgba(15, 118, 110, 0.1);
  stroke-width: 1;
}

.dashboard-page__map-grid path {
  fill: none;
  stroke: rgba(100, 116, 139, 0.18);
  stroke-dasharray: 4 10;
  stroke-linecap: round;
  vector-effect: non-scaling-stroke;
}

.dashboard-page__china-province {
  fill: url('#chinaMapLand');
  stroke: rgba(255, 255, 255, 0.9);
  stroke-linejoin: round;
  stroke-width: 1.15;
  vector-effect: non-scaling-stroke;
}

.dashboard-page__china-province:nth-child(3n + 1) {
  fill: #d3e8df;
}

.dashboard-page__china-province:nth-child(3n + 2) {
  fill: #c7ded5;
}

.dashboard-page__china-province:nth-child(3n) {
  fill: #bdd8ce;
}

.dashboard-page__map-badge {
  position: absolute;
  top: 12px;
  left: 12px;
  z-index: 2;
  display: grid;
  gap: 1px;
  min-width: 72px;
  padding: 6px 8px;
  border: 1px solid rgba(15, 118, 110, 0.18);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.74);
  box-shadow: 0 8px 20px rgba(15, 23, 42, 0.06);
  backdrop-filter: blur(8px);
}

.dashboard-page__map-badge span {
  color: #0f766e;
  font-size: 14px;
  font-weight: 800;
  line-height: 1;
}

.dashboard-page__map-badge small {
  color: var(--admin-muted);
  font-size: 10px;
  line-height: 1.25;
}

.dashboard-page__map-legend {
  position: absolute;
  right: 14px;
  bottom: 14px;
  z-index: 2;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  padding: 7px 9px;
  border: 1px solid rgba(15, 118, 110, 0.16);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.84);
  color: #334155;
  font-size: 12px;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.08);
  backdrop-filter: blur(8px);
}

.dashboard-page__map-legend span {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  line-height: 1;
}

.dashboard-page__map-dot {
  display: inline-block;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: #dc6a3d;
  box-shadow: 0 0 0 4px rgba(220, 106, 61, 0.14);
}

.dashboard-page__map-dot--private {
  background: #0f766e;
  box-shadow: 0 0 0 4px rgba(15, 118, 110, 0.14);
}

.dashboard-page__map-empty {
  position: absolute;
  top: 50%;
  left: 50%;
  z-index: 1;
  padding: 7px 12px;
  border: 1px solid rgba(100, 116, 139, 0.16);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.72);
  color: #64748b;
  font-size: 13px;
  transform: translate(-50%, -50%);
  pointer-events: none;
  backdrop-filter: blur(8px);
}

.dashboard-page__map-marker {
  position: absolute;
  z-index: 3;
  display: grid;
  min-width: 16px;
  min-height: 16px;
  padding: 0;
  place-items: center;
  border: 2px solid rgba(255, 255, 255, 0.9);
  border-radius: 999px;
  cursor: pointer;
  transform: translate(-50%, -50%);
  background:
    radial-gradient(circle at 38% 34%, rgba(255, 255, 255, 0.95), rgba(255, 255, 255, 0) 34%),
    rgba(220, 106, 61, var(--marker-alpha));
  box-shadow:
    0 0 0 5px rgba(220, 106, 61, 0.12),
    0 8px 18px rgba(127, 29, 29, 0.16);
  transition:
    box-shadow 0.18s ease,
    transform 0.18s ease;
}

.dashboard-page__map-marker::after {
  position: absolute;
  bottom: -3px;
  width: 7px;
  height: 7px;
  content: '';
  border-right: 2px solid rgba(255, 255, 255, 0.9);
  border-bottom: 2px solid rgba(255, 255, 255, 0.9);
  background: rgba(220, 106, 61, var(--marker-alpha));
  transform: rotate(45deg);
}

.dashboard-page__map-marker:hover,
.dashboard-page__map-marker:focus-visible {
  outline: 0;
  transform: translate(-50%, -54%) scale(1.06);
  box-shadow:
    0 0 0 7px rgba(220, 106, 61, 0.16),
    0 12px 24px rgba(127, 29, 29, 0.2);
}

.dashboard-page__map-marker--private {
  background:
    radial-gradient(circle at 38% 34%, rgba(255, 255, 255, 0.95), rgba(255, 255, 255, 0) 34%),
    rgba(15, 118, 110, var(--marker-alpha));
  box-shadow:
    0 0 0 5px rgba(15, 118, 110, 0.12),
    0 8px 18px rgba(19, 78, 74, 0.16);
}

.dashboard-page__map-marker--private::after {
  background: rgba(15, 118, 110, var(--marker-alpha));
}

.dashboard-page__map-marker--private:hover,
.dashboard-page__map-marker--private:focus-visible {
  box-shadow:
    0 0 0 7px rgba(15, 118, 110, 0.16),
    0 12px 24px rgba(19, 78, 74, 0.2);
}

.dashboard-page__map-marker span {
  position: absolute;
  top: -10px;
  right: -10px;
  z-index: 1;
  display: grid;
  min-width: 16px;
  height: 16px;
  padding: 0 4px;
  place-items: center;
  border: 1px solid rgba(220, 106, 61, 0.2);
  border-radius: 999px;
  color: #b45309;
  background: rgba(255, 255, 255, 0.9);
  box-shadow: 0 6px 14px rgba(15, 23, 42, 0.1);
  font-size: 10px;
  font-weight: 800;
  line-height: 1;
}

.dashboard-page__map-marker--private span {
  border-color: rgba(15, 118, 110, 0.22);
  color: #0f766e;
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

.dashboard-page__region b {
  display: block;
  width: 8%;
  height: 3px;
  overflow: hidden;
  border-radius: 999px;
  background: linear-gradient(90deg, #0f766e 0%, #dc6a3d 100%);
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
