'use client'
import { Suspense, useLayoutEffect, useRef } from 'react'
import { usePathname, useSearchParams } from 'next/navigation'
import { Spin, Tabs, TabPane } from '@douyinfe/semi-ui'
import { IconScissors } from '@douyinfe/semi-icons'
import { type WorkbenchTab, parseWorkbenchTab } from '@/app/lib/workbench'
import PageHeader from '../components/PageHeader'
import LiveMonitor from '@/app/ui/LiveMonitor'
import SessionsTab from './SessionsTab'
import FilesTab from './FilesTab'
import ClipsTab from './ClipsTab'
import dc from '@/app/ui/data-card.module.scss'
import styles from './page.module.scss'

/**
 * 剪辑台：「直播场次」（每场一行，从这里进回看页）、「录制文件」（按文件回放）、
 * 「切片管理」（精剪与拼接）和「实时监视」（正在录制的直播间多路同屏），当前 Tab 在查询串 `?tab=` 里。
 */
export default function Workbench() {
  return (
    <Suspense
      fallback={
        <div style={{ padding: '80px 0', textAlign: 'center' }}>
          <Spin size="large" />
        </div>
      }
    >
      <WorkbenchTabs />
    </Suspense>
  )
}

function WorkbenchTabs() {
  const pathname = usePathname()
  const params = useSearchParams()
  const tab = parseWorkbenchTab(params.get('tab'))
  const header = useRef<HTMLElement>(null)
  const content = useRef<HTMLDivElement>(null)
  useLayoutEffect(() => {
    const title = header.current
    const body = content.current
    if (!title || !body) return
    const measure = () => body.style.setProperty('--workbench-header-height', `${title.offsetHeight}px`)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(title)
    return () => observer.disconnect()
  }, [])

  const switchTab = (key: string) => {
    const next = new URLSearchParams(params.toString())
    next.set('tab', key)
    // 不走 router.replace：那是一次异步的软导航，切走「实时监视」时播放器要等导航提交才卸载，
    // 期间 mpegts.js 的销毁会和 MSE 回调撞上。原生 replaceState 由 Next 同步到 useSearchParams
    window.history.replaceState(null, '', `${pathname}?${next.toString()}`)
  }

  return (
    <>
      <PageHeader
        ref={header}
        icon={<IconScissors size="large" />}
        title="剪辑台"
        description="按场次回看、打标记和剪切片，或按文件在线回放；「实时监视」同屏查看正在录制的直播间"
      />
      <div className={dc.content} ref={content}>
        {/* keepDOM + lazyRender：场次和文件两个列表切走再切回，筛选、排序、分页、展开行都还在 */}
        <Tabs
          type="line"
          activeKey={tab}
          onChange={switchTab}
          className={styles.tabs}
          keepDOM
          lazyRender
        >
          <TabPane tab="直播场次" itemKey={'sessions' satisfies WorkbenchTab}>
            <SessionsTab />
          </TabPane>
          <TabPane tab="录制文件" itemKey={'files' satisfies WorkbenchTab}>
            <FilesTab />
          </TabPane>
          <TabPane tab="切片管理" itemKey={'clips' satisfies WorkbenchTab}>
            <ClipsTab active={tab === 'clips'} />
          </TabPane>
          <TabPane tab="实时监视" itemKey={'monitor' satisfies WorkbenchTab}>
            {/* 切走即卸载播放器、断开全部预览连接 */}
            {tab === 'monitor' ? <LiveMonitor /> : null}
          </TabPane>
        </Tabs>
      </div>
    </>
  )
}
