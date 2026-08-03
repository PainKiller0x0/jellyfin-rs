<script setup lang="ts">
import dayjs from 'dayjs';
import { computed, onMounted, ref, watch } from 'vue';

import * as serverApi from '@/services/server';
import { useAuthStore } from '@/stores/auth';
import type { PlaybackStats, PlaybackStatsItem, PlaybackStatsSeries, PlaybackStatsUser } from '@/types/server';

const authStore = useAuthStore();
const loading = ref(false);
const loadError = ref('');
const days = ref(30);
const stats = ref<PlaybackStats | null>(null);

const rangeOptions = [
  { label: '7天', value: 7 },
  { label: '30天', value: 30 },
  { label: '90天', value: 90 },
  { label: '365天', value: 365 }
];

const dailyRows = computed(() => stats.value?.Daily ?? []);
const topUsers = computed(() => stats.value?.Users ?? []);
const topSeries = computed(() => stats.value?.Series ?? []);
const topItems = computed(() => stats.value?.Items ?? []);
const maxDailyWatchSeconds = computed(() => Math.max(1, ...dailyRows.value.map(point => point.WatchSeconds)));
const maxUserWatchSeconds = computed(() => Math.max(1, ...topUsers.value.map(user => user.WatchSeconds)));
const maxSeriesWatchSeconds = computed(() => Math.max(1, ...topSeries.value.map(series => series.WatchSeconds)));
const maxItemWatchSeconds = computed(() => Math.max(1, ...topItems.value.map(item => item.WatchSeconds)));
const hasWatchData = computed(() => dailyRows.value.some(point => point.WatchSeconds > 0));

const cards = computed(() => [
  {
    label: '总观影时长',
    value: formatDuration(stats.value?.TotalWatchSeconds),
    hint: `${formatNumber(stats.value?.TotalPlayCount)} 次播放`
  },
  {
    label: '今日观影时长',
    value: formatDuration(stats.value?.TodayWatchSeconds),
    hint: dayjs().format('YYYY-MM-DD')
  },
  {
    label: '观看用户',
    value: formatNumber(stats.value?.UserCount),
    hint: `${formatNumber(stats.value?.ItemCount)} 个内容`
  }
]);

async function loadStats() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  loadError.value = '';
  try {
    stats.value = await serverApi.playbackStats(authStore.token, days.value);
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : '加载观影统计失败';
  } finally {
    loading.value = false;
  }
}

function formatNumber(value: number | undefined) {
  return typeof value === 'number' ? value.toLocaleString() : '-';
}

function formatDuration(value: number | undefined) {
  if (typeof value !== 'number') {
    return '-';
  }
  const seconds = Math.max(0, Math.round(value));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours > 0) {
    return `${hours.toLocaleString()}小时${minutes ? `${minutes}分` : ''}`;
  }
  if (minutes > 0) {
    return `${minutes}分钟`;
  }
  return `${seconds}秒`;
}

function itemTypeLabel(value: string) {
  const labels: Record<string, string> = {
    Movie: '电影',
    Series: '剧集',
    Season: '季',
    Episode: '单集',
    Video: '视频'
  };
  return labels[value] ?? value;
}

function dailyBarStyle(seconds: number) {
  const ratio = Math.min(1, seconds / maxDailyWatchSeconds.value);
  return {
    height: `${Math.max(4, ratio * 100)}%`
  };
}

function userBarStyle(user: PlaybackStatsUser) {
  return rankBarStyle(user.WatchSeconds, maxUserWatchSeconds.value);
}

function seriesBarStyle(series: PlaybackStatsSeries) {
  return rankBarStyle(series.WatchSeconds, maxSeriesWatchSeconds.value);
}

function itemBarStyle(item: PlaybackStatsItem) {
  return rankBarStyle(item.WatchSeconds, maxItemWatchSeconds.value);
}

function rankBarStyle(seconds: number, max: number) {
  const ratio = Math.min(1, seconds / max);
  return {
    width: `${Math.max(8, ratio * 100)}%`
  };
}

watch(days, loadStats);
onMounted(loadStats);
</script>

<template>
  <section class="admin-page playback-stats-page">
    <div class="playback-stats-page__heading">
      <div>
        <h1>观影统计</h1>
        <p>{{ days }} 天 · {{ formatDuration(stats?.TotalWatchSeconds) }}</p>
      </div>
      <div class="playback-stats-page__actions">
        <ElRadioGroup v-model="days" size="small">
          <ElRadioButton v-for="option in rangeOptions" :key="option.value" :label="option.value">
            {{ option.label }}
          </ElRadioButton>
        </ElRadioGroup>
        <ElButton :loading="loading" type="primary" @click="loadStats">
          <ElIcon>
            <Refresh />
          </ElIcon>
          刷新
        </ElButton>
      </div>
    </div>

    <ElAlert v-if="loadError" :closable="false" :title="loadError" type="error" />

    <div class="playback-stats-page__stats">
      <ElCard v-for="card in cards" :key="card.label" class="playback-stats-page__stat" shadow="never">
        <div class="playback-stats-page__stat-label">{{ card.label }}</div>
        <div class="playback-stats-page__stat-value">{{ card.value }}</div>
        <ElTag effect="plain">{{ card.hint }}</ElTag>
      </ElCard>
    </div>

    <div class="playback-stats-page__grid playback-stats-page__grid--main">
      <ElCard class="playback-stats-page__panel" shadow="never">
        <template #header>
          <div class="playback-stats-page__panel-title">
            <ElIcon>
              <DataAnalysis />
            </ElIcon>
            <span>按日图表</span>
          </div>
        </template>

        <div class="playback-stats-page__chart">
          <ElTooltip
            v-for="point in dailyRows"
            :key="point.Date"
            :content="`${point.Date} · ${formatDuration(point.WatchSeconds)} · ${point.PlayCount} 次播放`"
            placement="top"
            :show-after="120"
          >
            <div class="playback-stats-page__chart-day">
              <span class="playback-stats-page__chart-bar" :style="dailyBarStyle(point.WatchSeconds)"></span>
              <small>{{ dayjs(point.Date).format(days > 90 ? 'MM月' : 'MM-DD') }}</small>
            </div>
          </ElTooltip>
          <div v-if="stats && !hasWatchData" class="playback-stats-page__chart-empty">暂无观影时长</div>
        </div>
      </ElCard>

      <ElCard class="playback-stats-page__panel" shadow="never">
        <template #header>
          <div class="playback-stats-page__panel-title">
            <ElIcon>
              <User />
            </ElIcon>
            <span>用户观影时长</span>
          </div>
        </template>

        <div class="playback-stats-page__rank">
          <div v-for="user in topUsers" :key="user.UserId" class="playback-stats-page__rank-row">
            <div>
              <strong>{{ user.UserName || user.UserId }}</strong>
              <span>{{ user.PlayCount }} 次播放</span>
              <b :style="userBarStyle(user)"></b>
            </div>
            <ElTag effect="plain">{{ formatDuration(user.WatchSeconds) }}</ElTag>
          </div>
          <ElEmpty v-if="!topUsers.length" :image-size="80" description="暂无用户数据" />
        </div>
      </ElCard>
    </div>

    <div class="playback-stats-page__grid">
      <ElCard class="playback-stats-page__panel" shadow="never">
        <template #header>
          <div class="playback-stats-page__panel-title">
            <ElIcon>
              <Film />
            </ElIcon>
            <span>剧集观影时长</span>
          </div>
        </template>

        <div class="playback-stats-page__rank">
          <div v-for="series in topSeries" :key="series.SeriesId" class="playback-stats-page__rank-row">
            <div>
              <strong>{{ series.SeriesName || series.SeriesId }}</strong>
              <span>{{ series.ItemCount }} 个条目 · {{ series.PlayCount }} 次播放</span>
              <b :style="seriesBarStyle(series)"></b>
            </div>
            <ElTag effect="plain">{{ formatDuration(series.WatchSeconds) }}</ElTag>
          </div>
          <ElEmpty v-if="!topSeries.length" :image-size="80" description="暂无剧集数据" />
        </div>
      </ElCard>

      <ElCard class="playback-stats-page__panel" shadow="never">
        <template #header>
          <div class="playback-stats-page__panel-title">
            <ElIcon>
              <Tickets />
            </ElIcon>
            <span>内容观影时长</span>
          </div>
        </template>

        <div class="playback-stats-page__rank">
          <div v-for="item in topItems" :key="item.ItemId" class="playback-stats-page__rank-row">
            <div>
              <strong>{{ item.ItemName || item.ItemId }}</strong>
              <span>{{ itemTypeLabel(item.ItemType) }} · {{ item.SeriesName || '独立内容' }}</span>
              <b :style="itemBarStyle(item)"></b>
            </div>
            <ElTag effect="plain">{{ formatDuration(item.WatchSeconds) }}</ElTag>
          </div>
          <ElEmpty v-if="!topItems.length" :image-size="80" description="暂无内容数据" />
        </div>
      </ElCard>
    </div>
  </section>
</template>

<style scoped lang="scss">
.playback-stats-page {
  display: grid;
  gap: 18px;
}

.playback-stats-page__heading {
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

.playback-stats-page__actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  justify-content: flex-end;
}

.playback-stats-page__stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
  gap: 16px;
}

.playback-stats-page__stat {
  min-height: 128px;

  :deep(.el-card__body) {
    display: grid;
    min-height: 92px;
  }
}

.playback-stats-page__stat-label {
  color: var(--admin-muted);
  font-size: 13px;
}

.playback-stats-page__stat-value {
  margin: 12px 0 16px;
  font-size: 28px;
  font-weight: 800;
  line-height: 1.1;
}

.playback-stats-page__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.playback-stats-page__grid--main {
  grid-template-columns: minmax(0, 1.3fr) minmax(320px, 0.7fr);
}

.playback-stats-page__panel {
  min-width: 0;
}

.playback-stats-page__panel-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 700;
}

.playback-stats-page__chart {
  position: relative;
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(16px, 1fr));
  gap: 7px;
  align-items: end;
  min-height: 280px;
  padding: 16px 10px 8px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background:
    linear-gradient(180deg, rgba(37, 99, 235, 0.06), rgba(255, 255, 255, 0) 45%),
    var(--admin-surface);
}

.playback-stats-page__chart-day {
  display: grid;
  grid-template-rows: minmax(200px, 1fr) 22px;
  gap: 8px;
  min-width: 0;
  align-items: end;
}

.playback-stats-page__chart-bar {
  display: block;
  min-height: 4px;
  border-radius: 5px 5px 2px 2px;
  background: linear-gradient(180deg, #2563eb 0%, #0f766e 100%);
  box-shadow: 0 6px 14px rgba(37, 99, 235, 0.14);
}

.playback-stats-page__chart-day small {
  overflow: hidden;
  color: var(--admin-muted);
  font-size: 10px;
  line-height: 1.2;
  text-align: center;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.playback-stats-page__chart-empty {
  position: absolute;
  top: 50%;
  left: 50%;
  padding: 7px 12px;
  border: 1px solid rgba(100, 116, 139, 0.16);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.82);
  color: var(--admin-muted);
  font-size: 13px;
  transform: translate(-50%, -50%);
  pointer-events: none;
}

.playback-stats-page__rank {
  display: grid;
  gap: 10px;
}

.playback-stats-page__rank-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px;
  align-items: center;
  min-height: 58px;
  padding: 10px 12px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: var(--admin-surface);
}

.playback-stats-page__rank-row div {
  display: grid;
  min-width: 0;
  gap: 4px;
}

.playback-stats-page__rank-row strong,
.playback-stats-page__rank-row span {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.playback-stats-page__rank-row strong {
  font-size: 13px;
}

.playback-stats-page__rank-row span {
  color: var(--admin-muted);
  font-size: 12px;
}

.playback-stats-page__rank-row b {
  display: block;
  width: 8%;
  height: 3px;
  overflow: hidden;
  border-radius: 999px;
  background: linear-gradient(90deg, #2563eb 0%, #dc6a3d 100%);
}

@media (max-width: 980px) {
  .playback-stats-page__grid,
  .playback-stats-page__grid--main {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 640px) {
  .playback-stats-page__heading {
    flex-direction: column;
  }

  .playback-stats-page__actions {
    width: 100%;
    justify-content: flex-start;
  }

  .playback-stats-page__chart {
    grid-template-columns: repeat(auto-fit, minmax(12px, 1fr));
    min-height: 220px;
    gap: 5px;
  }

  .playback-stats-page__chart-day {
    grid-template-rows: minmax(160px, 1fr) 18px;
  }

  .playback-stats-page__chart-day small {
    display: none;
  }

  .playback-stats-page__rank-row {
    grid-template-columns: 1fr;
  }

  .playback-stats-page__rank-row .el-tag {
    justify-self: start;
  }
}
</style>
