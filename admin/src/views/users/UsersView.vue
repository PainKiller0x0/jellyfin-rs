<script setup lang="ts">
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus';
import { computed, onMounted, reactive, ref } from 'vue';

import * as usersApi from '@/services/users';
import { useAuthStore } from '@/stores/auth';
import type { JellyfinUser } from '@/types/jellyfin';

type CreateForm = {
  name: string;
  password: string;
};

type PasswordForm = {
  userId: string;
  userName: string;
  currentPassword: string;
  password: string;
};

const authStore = useAuthStore();
const loading = ref(false);
const saving = ref(false);
const actionUserId = ref('');
const userRows = ref<JellyfinUser[]>([]);
const createDialogVisible = ref(false);
const passwordDialogVisible = ref(false);
const createFormRef = ref<FormInstance>();
const passwordFormRef = ref<FormInstance>();

const createForm = reactive<CreateForm>({
  name: '',
  password: ''
});

const passwordForm = reactive<PasswordForm>({
  userId: '',
  userName: '',
  currentPassword: '',
  password: ''
});

const createRules: FormRules<CreateForm> = {
  name: [{ required: true, message: '请输入用户名', trigger: 'blur' }]
};

const passwordRules: FormRules<PasswordForm> = {
  password: [{ required: true, message: '请输入新密码', trigger: 'blur' }]
};

const adminCount = computed(() => userRows.value.filter(user => user.Policy?.IsAdministrator).length);
const disabledCount = computed(() => userRows.value.filter(user => user.Policy?.IsDisabled).length);
const passwordTargetIsSelf = computed(() => passwordForm.userId === authStore.user?.Id);

async function loadUsers() {
  if (!authStore.token) {
    return;
  }

  loading.value = true;
  try {
    userRows.value = await usersApi.users(authStore.token);
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '加载用户失败');
  } finally {
    loading.value = false;
  }
}

function resetCreateForm() {
  createForm.name = '';
  createForm.password = '';
  createFormRef.value?.clearValidate();
}

function openCreateDialog() {
  resetCreateForm();
  createDialogVisible.value = true;
}

function openPasswordDialog(user: JellyfinUser) {
  passwordForm.userId = user.Id;
  passwordForm.userName = user.Name;
  passwordForm.currentPassword = '';
  passwordForm.password = '';
  passwordFormRef.value?.clearValidate();
  passwordDialogVisible.value = true;
}

async function submitCreate() {
  const formEl = createFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }

  saving.value = true;
  try {
    await usersApi.createUser(authStore.token, {
      name: createForm.name,
      password: createForm.password
    });
    ElMessage.success('用户已创建');
    createDialogVisible.value = false;
    resetCreateForm();
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '创建用户失败');
  } finally {
    saving.value = false;
  }
}

async function submitPassword() {
  const formEl = passwordFormRef.value;
  if (!formEl || !authStore.token) {
    return;
  }

  const valid = await formEl.validate().catch(() => false);
  if (!valid) {
    return;
  }
  if (passwordTargetIsSelf.value && !passwordForm.currentPassword.trim()) {
    ElMessage.warning('请输入当前密码');
    return;
  }

  saving.value = true;
  try {
    await usersApi.updateUserPassword(
      authStore.token,
      passwordForm.userId,
      passwordForm.password,
      passwordForm.currentPassword
    );
    ElMessage.success('密码已更新');
    passwordDialogVisible.value = false;
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '更新密码失败');
  } finally {
    saving.value = false;
  }
}

async function toggleAdmin(user: JellyfinUser) {
  await updatePolicy(user, { IsAdministrator: !user.Policy?.IsAdministrator }, '权限已更新');
}

async function toggleDisabled(user: JellyfinUser) {
  await updatePolicy(user, { IsDisabled: !user.Policy?.IsDisabled }, user.Policy?.IsDisabled ? '用户已启用' : '用户已禁用');
}

async function updatePolicy(user: JellyfinUser, payload: usersApi.UpdateUserPolicyPayload, message: string) {
  if (!authStore.token) {
    return;
  }

  actionUserId.value = user.Id;
  try {
    await usersApi.updateUserPolicy(authStore.token, user.Id, payload);
    ElMessage.success(message);
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '更新用户失败');
  } finally {
    actionUserId.value = '';
  }
}

async function removeUser(user: JellyfinUser) {
  if (!authStore.token || user.Id === authStore.user?.Id) {
    return;
  }

  const confirmed = await ElMessageBox.confirm(`确认删除用户 "${user.Name}"？`, '删除用户', {
    type: 'warning',
    confirmButtonText: '删除',
    cancelButtonText: '取消'
  })
    .then(() => true)
    .catch(() => false);
  if (!confirmed) {
    return;
  }

  actionUserId.value = user.Id;
  try {
    await usersApi.deleteUser(authStore.token, user.Id);
    ElMessage.success('用户已删除');
    await loadUsers();
  } catch (error) {
    ElMessage.error(error instanceof Error ? error.message : '删除用户失败');
  } finally {
    actionUserId.value = '';
  }
}

onMounted(loadUsers);
</script>

<template>
  <section class="admin-page users-page">
    <div class="users-page__heading">
      <div>
        <h1>用户</h1>
        <p>{{ userRows.length }} 个用户，{{ adminCount }} 个管理员，{{ disabledCount }} 个已禁用。</p>
      </div>
      <ElSpace wrap>
        <ElButton :loading="loading" @click="loadUsers">
          <ElIcon>
            <Refresh />
          </ElIcon>
          刷新
        </ElButton>
        <ElButton type="primary" @click="openCreateDialog">
          <ElIcon>
            <Plus />
          </ElIcon>
          新建用户
        </ElButton>
      </ElSpace>
    </div>

    <ElCard shadow="never">
      <ElTable v-loading="loading" :data="userRows" empty-text="暂无用户">
        <ElTableColumn label="用户名" min-width="180">
          <template #default="{ row }">
            <div class="users-page__identity">
              <ElAvatar :size="34">{{ row.Name.slice(0, 1).toUpperCase() }}</ElAvatar>
              <div>
                <strong>{{ row.Name }}</strong>
                <span>{{ row.Id }}</span>
              </div>
            </div>
          </template>
        </ElTableColumn>
        <ElTableColumn label="角色" width="120">
          <template #default="{ row }">
            <ElTag :type="row.Policy?.IsAdministrator ? 'success' : 'info'" effect="plain">
              {{ row.Policy?.IsAdministrator ? '管理员' : '用户' }}
            </ElTag>
          </template>
        </ElTableColumn>
        <ElTableColumn label="状态" width="110">
          <template #default="{ row }">
            <ElTag :type="row.Policy?.IsDisabled ? 'danger' : 'success'" effect="plain">
              {{ row.Policy?.IsDisabled ? '已禁用' : '正常' }}
            </ElTag>
          </template>
        </ElTableColumn>
        <ElTableColumn label="密码" width="110">
          <template #default="{ row }">
            <ElTag :type="row.HasPassword ? 'success' : 'warning'" effect="plain">
              {{ row.HasPassword ? '已配置' : '未配置' }}
            </ElTag>
          </template>
        </ElTableColumn>
        <ElTableColumn align="right" label="操作" min-width="260">
          <template #default="{ row }">
            <ElButton link type="primary" @click="openPasswordDialog(row)">重置密码</ElButton>
            <ElButton :loading="actionUserId === row.Id" link type="primary" @click="toggleAdmin(row)">
              {{ row.Policy?.IsAdministrator ? '取消管理员' : '设为管理员' }}
            </ElButton>
            <ElButton :loading="actionUserId === row.Id" link type="warning" @click="toggleDisabled(row)">
              {{ row.Policy?.IsDisabled ? '启用' : '禁用' }}
            </ElButton>
            <ElButton
              :disabled="row.Id === authStore.user?.Id"
              :loading="actionUserId === row.Id"
              link
              type="danger"
              @click="removeUser(row)"
            >
              删除
            </ElButton>
          </template>
        </ElTableColumn>
      </ElTable>
    </ElCard>

    <ElDialog v-model="createDialogVisible" title="新建用户" width="460px" @closed="resetCreateForm">
      <ElForm ref="createFormRef" :model="createForm" :rules="createRules" label-position="top">
        <ElFormItem label="用户名" prop="name">
          <ElInput v-model.trim="createForm.name" />
        </ElFormItem>
        <ElFormItem label="密码">
          <ElInput v-model="createForm.password" show-password type="password" />
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="createDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitCreate">保存</ElButton>
      </template>
    </ElDialog>

    <ElDialog v-model="passwordDialogVisible" title="重置密码" width="460px">
      <ElForm ref="passwordFormRef" :model="passwordForm" :rules="passwordRules" label-position="top">
        <ElFormItem label="用户">
          <ElInput v-model="passwordForm.userName" disabled />
        </ElFormItem>
        <ElFormItem v-if="passwordTargetIsSelf" label="当前密码">
          <ElInput v-model="passwordForm.currentPassword" show-password type="password" />
        </ElFormItem>
        <ElFormItem label="新密码" prop="password">
          <ElInput v-model="passwordForm.password" show-password type="password" />
        </ElFormItem>
      </ElForm>
      <template #footer>
        <ElButton @click="passwordDialogVisible = false">取消</ElButton>
        <ElButton :loading="saving" type="primary" @click="submitPassword">保存</ElButton>
      </template>
    </ElDialog>
  </section>
</template>

<style scoped lang="scss">
.users-page {
  display: grid;
  gap: 18px;
}

.users-page__heading {
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

.users-page__identity {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;

  strong,
  span {
    display: block;
    overflow: hidden;
    max-width: 280px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  span {
    color: var(--admin-muted);
    font-size: 12px;
  }
}

@media (max-width: 760px) {
  .users-page__heading {
    flex-direction: column;
  }
}
</style>
