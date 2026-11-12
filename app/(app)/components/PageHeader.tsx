'use client'
import { Typography } from '@douyinfe/semi-ui'
import type { ReactNode, Ref } from 'react'
import styles from './PageHeader.module.scss'

const { Title, Text } = Typography

/**
 * 统一的页面头部范式：标题 + 描述 + 右侧操作区。
 * 所有业务页面都应以 <PageHeader /> 开头，保证全局一致。
 */
export default function PageHeader({
  title,
  description,
  icon,
  actions,
  ref,
}: {
  title: ReactNode
  description?: ReactNode
  icon?: ReactNode
  actions?: ReactNode
  ref?: Ref<HTMLElement>
}) {
  return (
    <header className={styles.header} ref={ref}>
      {icon ? <div className={styles.icon}>{icon}</div> : null}
      <div className={styles.titles}>
        <Title heading={4} style={{ margin: 0 }}>
          {title}
        </Title>
        {description ? (
          <Text type="tertiary" size="small">
            {description}
          </Text>
        ) : null}
      </div>
      {actions ? <div className={styles.actions}>{actions}</div> : null}
    </header>
  )
}
