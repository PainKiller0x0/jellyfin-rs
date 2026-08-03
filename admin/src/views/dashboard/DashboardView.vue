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
const GITHUB_HEAT_COLORS = ['#ebedf0', '#9be9a8', '#40c463', '#30a14e', '#216e39'] as const;
const PROVINCE_CODE_ALIASES: Record<string, string> = {
  bj: '11',
  beijing: '11',
  tj: '12',
  tianjin: '12',
  hebei: '13',
  shanxi: '14',
  sx: '14',
  neimenggu: '15',
  inner_mongolia: '15',
  liaoning: '21',
  jilin: '22',
  heilongjiang: '23',
  shanghai: '31',
  jiangsu: '32',
  zhejiang: '33',
  anhui: '34',
  fujian: '35',
  jiangxi: '36',
  shandong: '37',
  henan: '41',
  hubei: '42',
  hunan: '43',
  guangdong: '44',
  guangxi: '45',
  hainan: '46',
  chongqing: '50',
  sichuan: '51',
  guizhou: '52',
  yunnan: '53',
  xizang: '54',
  tibet: '54',
  shaanxi: '61',
  sn: '61',
  gansu: '62',
  qinghai: '63',
  ningxia: '64',
  xinjiang: '65',
  taiwan: '71',
  hongkong: '香港',
  hong_kong: '香港',
  macau: '澳门',
  macao: '澳门'
};
type ChinaProvinceProperties = GeoJsonProperties & {
  id?: string;
  name?: string;
};
type ChinaProvincePath = {
  id: string;
  name: string;
  d: string;
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
const chinaProvincePaths: ChinaProvincePath[] = chinaFeatures.map(feature => ({
  id: feature.properties?.id ?? feature.properties?.name,
  name: feature.properties?.name ?? '',
  d: chinaPath(feature) ?? ''
})).filter((province): province is ChinaProvincePath => Boolean(province.id && province.name && province.d));
const chinaProvinceIds = new Set(chinaProvincePaths.map(province => province.id));
const chinaProvinceNameToId = new Map(
  chinaProvincePaths.map(province => [normalizeProvinceName(province.name), province.id])
);

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
const totalViewerCount = computed(() => regionRows.value.reduce((total, region) => total + region.UserCount, 0));
const maxRegionUsers = computed(() => Math.max(1, ...regionRows.value.map(region => region.UserCount)));
const provinceRegionGroups = computed(() => {
  const groups = new Map<
    string,
    {
      viewerCount: number;
      playCount: number;
      ipCount: number;
      regions: PlaybackRegion[];
    }
  >();

  for (const region of regionRows.value) {
    const provinceId = provinceIdForRegion(region);
    if (!provinceId) {
      continue;
    }

    const group = groups.get(provinceId) ?? {
      viewerCount: 0,
      playCount: 0,
      ipCount: 0,
      regions: []
    };
    group.viewerCount += region.UserCount;
    group.playCount += region.PlayCount;
    group.ipCount += region.IpCount;
    group.regions.push(region);
    groups.set(provinceId, group);
  }

  return groups;
});
const maxProvinceUsers = computed(() =>
  Math.max(1, ...Array.from(provinceRegionGroups.value.values()).map(group => group.viewerCount))
);
const provinceHeatPaths = computed(() =>
  chinaProvincePaths.map(province => {
    const group = provinceRegionGroups.value.get(province.id);
    const viewerCount = group?.viewerCount ?? 0;
    const playCount = group?.playCount ?? 0;
    const ipCount = group?.ipCount ?? 0;
    const level = heatLevel(viewerCount, maxProvinceUsers.value);
    return {
      ...province,
      viewerCount,
      playCount,
      ipCount,
      color: GITHUB_HEAT_COLORS[level],
      level,
      tooltip: viewerCount
        ? `${province.name} · ${viewerCount} 用户 · ${playCount} 次播放 · ${ipCount} IP`
        : `${province.name} · 暂无观看`
    };
  })
);
const hasMappedProvince = computed(() => provinceHeatPaths.value.some(province => province.viewerCount > 0));
const mapSummary = computed(() => `用户分布地图，${formatNumber(totalViewerCount.value)} 个观看用户计数`);

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

function regionTooltip(region: PlaybackRegion) {
  const network = region.IsPrivate ? '内网' : '公网';
  return `${region.Region} · ${network} · ${region.PlayCount} 次播放 · ${region.UserCount} 用户`;
}

function normalizeProvinceName(value: string) {
  return value
    .trim()
    .replace(/特别行政区|壮族自治区|回族自治区|维吾尔自治区|自治区|省|市/g, '')
    .replace(/[\s._-]/g, '')
    .toLowerCase();
}

function provinceIdForRegion(region: PlaybackRegion) {
  if (region.IsPrivate) {
    return undefined;
  }

  const candidates = [region.ProvinceCode, region.ProvinceName, region.RegionCode, region.Region].filter(
    (value): value is string => Boolean(value)
  );
  for (const candidate of candidates) {
    const value = candidate.trim();
    if (chinaProvinceIds.has(value)) {
      return value;
    }

    const codeMatch = value.match(/^(?:cn[-_:]?|province[-_:]?|region[-_:]?)?(\d{2})$/i);
    if (codeMatch && chinaProvinceIds.has(codeMatch[1])) {
      return codeMatch[1];
    }

    const alias = PROVINCE_CODE_ALIASES[value.toLowerCase()];
    if (alias && chinaProvinceIds.has(alias)) {
      return alias;
    }

    const normalized = normalizeProvinceName(value);
    const nameMatch = chinaProvinceNameToId.get(normalized);
    if (nameMatch) {
      return nameMatch;
    }

    const containingName = chinaProvincePaths.find(province => {
      const provinceName = normalizeProvinceName(province.name);
      return normalized.length >= 2 && normalized.includes(provinceName);
    });
    if (containingName) {
      return containingName.id;
    }
  }

  return undefined;
}

function heatLevel(value: number, max: number) {
  if (value <= 0) {
    return 0;
  }

  const ratio = value / Math.max(1, max);
  if (ratio <= 0.25) {
    return 1;
  }
  if (ratio <= 0.5) {
    return 2;
  }
  if (ratio <= 0.75) {
    return 3;
  }
  return 4;
}

function githubHeatColor(value: number, max: number) {
  return GITHUB_HEAT_COLORS[heatLevel(value, max)];
}

function regionBarStyle(region: PlaybackRegion) {
  const ratio = Math.min(1, region.UserCount / maxRegionUsers.value);
  return {
    width: `${Math.max(8, ratio * 100)}%`,
    background: githubHeatColor(region.UserCount, maxRegionUsers.value)
  };
}

function regionHeatStyle(region: PlaybackRegion) {
  return {
    background: githubHeatColor(region.UserCount, maxRegionUsers.value)
  };
}

function regionMeta(region: PlaybackRegion) {
  const location = [region.ProvinceName, region.CityName, region.Isp].filter(Boolean).join(' · ');
  return [`${region.UserCount} 用户`, `${region.IpCount} IP`, location, region.SampleIps.join(', ')]
    .filter(Boolean)
    .join(' · ');
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
              role="img"
              :aria-label="mapSummary"
            >
              <defs>
                <filter id="mapLandShadow" x="-3%" y="-6%" width="106%" height="112%">
                  <feDropShadow dx="0" dy="3" flood-color="#1f2328" flood-opacity="0.06" stdDeviation="4" />
                </filter>
              </defs>
              <rect class="dashboard-page__map-ocean" :width="CHINA_MAP_WIDTH" :height="CHINA_MAP_HEIGHT" rx="18" />
              <g class="dashboard-page__map-grid">
                <path d="M110 98H900M110 182H900M110 266H900M110 350H900M110 434H900" />
                <path d="M188 62V455M326 62V455M464 62V455M602 62V455M740 62V455M878 62V455" />
              </g>
              <g class="dashboard-page__china-map" filter="url(#mapLandShadow)">
                <path
                  v-for="province in provinceHeatPaths"
                  :key="province.id"
                  class="dashboard-page__china-province"
                  :class="{ 'dashboard-page__china-province--active': province.viewerCount > 0 }"
                  :d="province.d"
                  :data-level="province.level"
                  :style="{ fill: province.color }"
                  :tabindex="province.viewerCount > 0 ? 0 : -1"
                  :aria-label="province.tooltip"
                >
                  <title>{{ province.tooltip }}</title>
                </path>
              </g>
            </svg>
            <div class="dashboard-page__map-badge">
              <span>{{ formatNumber(totalViewerCount) }}</span>
              <small>观看用户计数</small>
            </div>
            <div class="dashboard-page__map-legend" aria-label="观看用户计数颜色图例">
              <span>少</span>
              <i
                v-for="color in GITHUB_HEAT_COLORS"
                :key="color"
                class="dashboard-page__map-legend-cell"
                :style="{ background: color }"
              ></i>
              <span>多</span>
            </div>
            <div v-if="!regionRows.length" class="dashboard-page__map-empty">暂无播放分布</div>
            <div v-else-if="!hasMappedProvince" class="dashboard-page__map-empty">暂无省份归属</div>
          </div>
          <div class="dashboard-page__region-list">
            <div v-for="region in regionRows.slice(0, 8)" :key="region.RegionCode" class="dashboard-page__region">
              <i class="dashboard-page__region-swatch" :style="regionHeatStyle(region)"></i>
              <div>
                <strong>{{ region.Region }}</strong>
                <span>{{ regionMeta(region) }}</span>
                <b :style="regionBarStyle(region)"></b>
              </div>
              <ElTag class="dashboard-page__region-count" type="success" effect="plain">
                {{ region.UserCount }} 人 / {{ region.PlayCount }} 次
              </ElTag>
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
  background:
    linear-gradient(rgba(31, 35, 40, 0.08) 1px, transparent 1px),
    linear-gradient(90deg, rgba(31, 35, 40, 0.08) 1px, transparent 1px),
    var(--admin-surface);
  background-size:
    100% 74px,
    150px 100%,
    auto;
  isolation: isolate;
}

.dashboard-page__map-canvas {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
}

.dashboard-page__map-ocean {
  fill: transparent;
  stroke: none;
}

.dashboard-page__map-grid {
  display: none;
}

.dashboard-page__china-province {
  fill: var(--admin-surface-soft);
  stroke: var(--admin-border);
  stroke-linejoin: round;
  stroke-width: 1;
  vector-effect: non-scaling-stroke;
  transition:
    filter 0.16s ease,
    stroke 0.16s ease,
    stroke-width 0.16s ease;
}

.dashboard-page__china-province--active {
  cursor: pointer;
}

.dashboard-page__china-province--active:hover,
.dashboard-page__china-province--active:focus-visible {
  outline: 0;
  filter: brightness(0.96);
  stroke: var(--admin-text);
  stroke-width: 1.5;
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
  border: 1px solid rgba(31, 35, 40, 0.12);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.82);
  box-shadow: 0 8px 20px rgba(15, 23, 42, 0.06);
  backdrop-filter: blur(8px);
}

.dashboard-page__map-badge span {
  color: #216e39;
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
  gap: 5px;
  align-items: center;
  padding: 7px 8px;
  border: 1px solid rgba(31, 35, 40, 0.12);
  border-radius: 8px;
  background: var(--admin-surface-soft);
  color: var(--admin-muted);
  font-size: 11px;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.08);
  backdrop-filter: blur(8px);
}

.dashboard-page__map-legend span {
  line-height: 1;
}

.dashboard-page__map-legend-cell {
  display: block;
  width: 11px;
  height: 11px;
  border: 1px solid rgba(31, 35, 40, 0.08);
  border-radius: 2px;
}

.dashboard-page__map-empty {
  position: absolute;
  top: 50%;
  left: 50%;
  z-index: 1;
  padding: 7px 12px;
  border: 1px solid rgba(100, 116, 139, 0.16);
  border-radius: 999px;
  background: var(--admin-surface-soft);
  color: var(--admin-muted);
  font-size: 13px;
  transform: translate(-50%, -50%);
  pointer-events: none;
  backdrop-filter: blur(8px);
}

.dashboard-page__region-list {
  display: grid;
  gap: 10px;
}

.dashboard-page__region {
  display: grid;
  grid-template-columns: 14px minmax(0, 1fr) auto;
  gap: 12px;
  align-items: center;
  padding: 10px 12px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: var(--admin-surface);
}

.dashboard-page__region-swatch {
  width: 12px;
  height: 12px;
  border: 1px solid rgba(31, 35, 40, 0.08);
  border-radius: 3px;
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
  background: #40c463;
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
    grid-template-columns: 14px minmax(0, 1fr);
  }

  .dashboard-page__region-count {
    grid-column: 2;
    justify-self: start;
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
