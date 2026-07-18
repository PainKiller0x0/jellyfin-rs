<script setup lang="ts">
import { useRouter } from 'vue-router';

const router = useRouter();
</script>

<template>
  <section class="not-found-page">
    <ElResult title="404" sub-title="页面不存在或已经移动">
      <template #icon>
        <ElIcon class="not-found-page__icon">
          <Warning />
        </ElIcon>
      </template>
      <template #extra>
        <ElButton type="primary" @click="router.push('/')">返回控制台</ElButton>
      </template>
    </ElResult>
  </section>
</template>

<style scoped lang="scss">
.not-found-page {
  display: grid;
  min-height: 100vh;
  place-items: center;
  background: var(--admin-bg);
}

.not-found-page__icon {
  color: var(--admin-accent);
  font-size: 64px;
}
</style>
