import { Modal } from '@douyinfe/semi-ui'
import type { ReactNode } from 'react'

export default function TrashConfirmation({
  visible, title, pending, onConfirm, onCancel, children, confirmText = '移入废纸篓',
}: {
  visible: boolean
  title: string
  confirmText?: string
  pending: boolean
  onConfirm: () => void
  onCancel: () => void
  children: ReactNode
}) {
  return (
    <Modal
      visible={visible}
      title={title}
      okText={confirmText}
      cancelText="取消"
      okButtonProps={{ type: 'danger', 'aria-label': confirmText }}
      cancelButtonProps={{ disabled: pending, 'aria-label': '取消' }}
      confirmLoading={pending}
      closable={!pending}
      closeOnEsc={!pending}
      maskClosable={false}
      onOk={onConfirm}
      onCancel={() => {
        if (!pending) onCancel()
      }}
    >
      {children}
    </Modal>
  )
}
