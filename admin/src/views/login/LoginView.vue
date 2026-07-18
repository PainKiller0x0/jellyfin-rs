<script setup lang="ts">
import { Lock, User } from '@element-plus/icons-vue';
import { ElMessage, type FormInstance, type FormRules } from 'element-plus';
import { reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';

import { useAuthStore } from '@/stores/auth';

type LoginForm = {
  username: string;
  password: string;
};

const route = useRoute();
const router = useRouter();
const authStore = useAuthStore();
const formRef = ref<FormInstance>();
const loading = ref(false);

const form = reactive<LoginForm>({
  username: '',
  password: ''
});

const rules: FormRules<LoginForm> = {
  username: [{ required: true, message: '请输入用户名', trigger: 'blur' }],
  password: [{ required: true, message: '请输入密码', trigger: 'blur' }]
};

async function submit() {
  const formEl = formRef.value;
  if (!formEl) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  loading.value = true;
  try {
    await authStore.login(form.username, form.password);
    const redirect = typeof route.query.redirect === 'string' ? route.query.redirect : '/';
    await router.replace(redirect);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '登录失败');
  } finally {
    loading.value = false;
  }
}
</script>

<template>
  <main class="login-page">
    <section class="login-page__panel">
      <div class="login-page__brand">
        <div class="login-page__logo">JR</div>
        <div>
          <h1>jellyfin-rs</h1>
          <p>管理后台</p>
        </div>
      </div>

      <ElForm ref="formRef" :model="form" :rules="rules" size="large" @submit.prevent="submit">
        <ElFormItem prop="username">
          <ElInput v-model.trim="form.username" autocomplete="username" placeholder="用户名">
            <template #prefix>
              <ElIcon>
                <User />
              </ElIcon>
            </template>
          </ElInput>
        </ElFormItem>

        <ElFormItem prop="password">
          <ElInput
            v-model="form.password"
            autocomplete="current-password"
            placeholder="密码"
            show-password
            type="password"
          >
            <template #prefix>
              <ElIcon>
                <Lock />
              </ElIcon>
            </template>
          </ElInput>
        </ElFormItem>

        <ElButton class="login-page__submit" :loading="loading" native-type="submit" type="primary">
          登录
        </ElButton>
      </ElForm>
    </section>
  </main>
</template>

<style scoped lang="scss">
.login-page {
  display: grid;
  min-height: 100vh;
  place-items: center;
  padding: 24px;
  background:
    linear-gradient(135deg, rgb(15 118 110 / 12%), transparent 34%),
    linear-gradient(315deg, rgb(180 83 9 / 14%), transparent 30%),
    #f6f8fb;
}

.login-page__panel {
  width: min(420px, 100%);
  padding: 28px;
  border: 1px solid var(--admin-border);
  border-radius: 8px;
  background: #ffffff;
  box-shadow: 0 20px 55px rgb(15 23 42 / 10%);
}

.login-page__brand {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 28px;

  h1 {
    margin: 0;
    font-size: 24px;
    line-height: 1.1;
  }

  p {
    margin: 5px 0 0;
    color: var(--admin-muted);
    font-size: 13px;
  }
}

.login-page__logo {
  display: grid;
  width: 48px;
  height: 48px;
  place-items: center;
  border-radius: 8px;
  color: #ffffff;
  background: linear-gradient(135deg, #0f766e, #2563eb);
  font-size: 16px;
  font-weight: 800;
}

.login-page__submit {
  width: 100%;
}
</style>
