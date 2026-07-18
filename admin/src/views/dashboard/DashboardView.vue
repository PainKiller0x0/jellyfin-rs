<script setup lang="ts">
const cards = [
  { label: '服务状态', value: '等待接入', tone: 'success' },
  { label: '媒体项目', value: '-', tone: 'primary' },
  { label: '活跃会话', value: '-', tone: 'warning' },
  { label: '计划任务', value: '-', tone: 'info' }
];
</script>

<template>
  <section class="admin-page dashboard-page">
    <div class="dashboard-page__heading">
      <div>
        <h1>控制台</h1>
        <p>管理媒体库、用户、任务和服务器配置。</p>
      </div>
      <ElButton type="primary">
        <ElIcon>
          <Refresh />
        </ElIcon>
        刷新
      </ElButton>
    </div>

    <div class="dashboard-page__stats">
      <ElCard v-for="card in cards" :key="card.label" class="dashboard-page__stat" shadow="never">
        <div class="dashboard-page__stat-label">{{ card.label }}</div>
        <div class="dashboard-page__stat-value">{{ card.value }}</div>
        <ElTag :type="card.tone" effect="plain">待连接真实接口</ElTag>
      </ElCard>
    </div>

    <ElCard class="dashboard-page__panel" shadow="never">
      <template #header>
        <div class="dashboard-page__panel-title">
          <ElIcon>
            <Operation />
          </ElIcon>
          <span>管理功能规划</span>
        </div>
      </template>

      <ElTimeline>
        <ElTimelineItem timestamp="第一阶段" type="success">后台工程骨架、布局和路由</ElTimelineItem>
        <ElTimelineItem timestamp="第二阶段" type="primary">登录认证、Token 持久化和接口层</ElTimelineItem>
        <ElTimelineItem timestamp="第三阶段" type="warning">系统概览、媒体库、计划任务、用户与配置管理</ElTimelineItem>
      </ElTimeline>
    </ElCard>
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

@media (max-width: 980px) {
  .dashboard-page__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
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
