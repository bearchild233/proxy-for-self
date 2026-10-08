<script setup lang="ts" generic="Row extends object = Record<string, unknown>">
import type { BaseTableProps, BaseTableSort, ResolvedTableColumn } from './columns'

import { ChevronLeft, ChevronRight, GripVertical, Triangle } from '@lucide/vue'
import { useResizeObserver } from '@vueuse/core'
import { useSortable } from '@vueuse/integrations/useSortable'
import { computed, nextTick, onMounted, shallowRef, useSlots, useTemplateRef, watch } from 'vue'
import BaseButton from '../BaseButton.vue'
import BaseEmpty from '../BaseEmpty.vue'
import BaseScrollbar from '../BaseScrollbar.vue'
import {
  alignClass,
  cellContentClass,
  cellDisplayValue,
  cellTitle,
  cellValue,
  columnSortKey,
  columnStyle,
  resolveColumns,
  stickyStyle,
  tableStyle,
} from './columns'
import { useTableColumnMotion } from './useTableColumnMotion'
import { useTableExpansionMotion } from './useTableExpansionMotion'

const props = withDefaults(defineProps<BaseTableProps<Row>>(), {
  rowKey: 'id',
  selectedRowKeys: () => [],
  expandedRowKeys: () => [],
  density: 'default',
  loading: false,
  emptyText: '暂无数据',
  showHeaderWhenEmpty: false,
  scrollbarAlwaysVisible: false,
  sort: undefined,
})

const emit = defineEmits<{
  sortChange: [sort: BaseTableSort | undefined]
  reorder: [change: { originalIds: string[], orderedIds: string[] }]
}>()
const slots = useSlots()
const computedColumns = computed(() => resolveColumns(props.reorderable ? [{ key: '__reorder', label: '排序', kind: 'expander' }, ...props.columns] : props.columns))
const resolvedTableStyle = computed(() => tableStyle(computedColumns.value))

const retainedRows = shallowRef<Row[]>([])
watch(
  [() => props.rows, () => props.loading],
  ([rows, loading]) => {
    if (rows.length > 0 || !loading)
      retainedRows.value = rows
  },
  { immediate: true },
)
const dragRows = shallowRef<Row[]>()
const displayRows = computed(() => dragRows.value ?? (props.loading && props.rows.length === 0 ? retainedRows.value : props.rows))
const hasRows = computed(() => displayRows.value.length > 0)

const scrollbarRef = useTemplateRef<InstanceType<typeof BaseScrollbar>>('scrollbar')
const tableRef = useTemplateRef<HTMLTableElement>('table')
const canReorder = computed(() => props.reorderable && !props.loading && !props.reorderDisabled && !props.sort)
function moveRow(from: number, to: number, rows = displayRows.value) {
  if (from === to || to < 0 || to >= rows.length)
    return
  const originalIds = rows.map((row, index) => String(getRowKey(row, index)))
  const orderedIds = [...originalIds]
  orderedIds.splice(to, 0, orderedIds.splice(from, 1)[0]!)
  emit('reorder', { originalIds, orderedIds })
}
function moveWithKeyboard(event: KeyboardEvent, index: number) {
  if (canReorder.value && event.altKey && ['ArrowUp', 'ArrowDown'].includes(event.key)) {
    event.preventDefault()
    moveRow(index, index + (event.key === 'ArrowUp' ? -1 : 1))
  }
}
const sortable = useSortable(tableRef, shallowRef<Row[]>([]), {
  watchElement: true,
  animation: 150,
  forceFallback: true,
  fallbackTolerance: 3,
  handle: '[data-row-handle]',
  draggable: 'tbody[data-row-group]',
  ghostClass: 'opacity-40',
  disabled: !canReorder.value,
  onStart: () => { dragRows.value = [...displayRows.value] },
  onUpdate: () => {},
  onEnd: (event) => {
    // Sortable 只提供手势，先还原 DOM，再交给 Vue 和服务器更新（展开行与主行一起移动）。
    const rows = dragRows.value ?? displayRows.value
    event.item.remove()
    event.from.insertBefore(event.item, event.from.children[event.oldIndex ?? 0] ?? null)
    dragRows.value = undefined
    if (event.oldDraggableIndex !== undefined && event.newDraggableIndex !== undefined)
      moveRow(event.oldDraggableIndex, event.newDraggableIndex, rows)
  },
})
watch(canReorder, value => sortable.option('disabled', !value))
useTableColumnMotion(tableRef, () => computedColumns.value.map(column => column.key))
const horizontalScrolled = shallowRef(false)
const horizontalCanScrollRight = shallowRef(false)

function scrollHorizontally(direction: number) {
  const wrap = scrollbarRef.value?.wrapRef
  if (wrap)
    wrap.scrollBy({ left: direction * Math.max(240, wrap.clientWidth * 0.65), behavior: 'smooth' })
}

function updateScrollLayout() {
  scrollbarRef.value?.update()
  const wrap = scrollbarRef.value?.wrapRef
  if (!wrap) {
    horizontalScrolled.value = false
    horizontalCanScrollRight.value = false
    return
  }
  const range = Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
  horizontalScrolled.value = wrap.scrollLeft > 1
  horizontalCanScrollRight.value = wrap.scrollLeft < range - 1
}

function handleTableScroll(payload: { scrollTop: number, scrollLeft: number }) {
  const wrap = scrollbarRef.value?.wrapRef
  if (!wrap)
    return
  const range = Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
  horizontalScrolled.value = payload.scrollLeft > 1
  horizontalCanScrollRight.value = payload.scrollLeft < range - 1
}

onMounted(async () => {
  await nextTick()
  updateScrollLayout()
})
useResizeObserver(() => [scrollbarRef.value?.wrapRef, tableRef.value].filter(Boolean), updateScrollLayout)
watch([() => displayRows.value.length, () => props.columns], async () => {
  await nextTick()
  updateScrollLayout()
})

const headerRowClass = computed(() => [
  props.density === 'compact' ? 'h-8 text-cp-xs' : 'h-10 text-cp-sm',
  'font-bold text-cp-text-secondary',
])
const bodyRowClass = computed(() => (props.density === 'compact' ? 'h-cp-table-row-sm' : 'h-cp-table-row'))
const cellPaddingClass = computed(() => (props.density === 'compact' ? 'px-3' : 'px-4'))
const bodyTextClass = computed(() => (props.density === 'compact' ? 'text-cp-sm' : 'text-cp'))
const bodyCellFrameClass = computed(() =>
  props.density === 'compact'
    ? 'border-y-2 border-transparent bg-clip-padding'
    : 'border-y-[3px] border-transparent bg-clip-padding',
)
const firstRowTopGapClass = computed(() => (props.density === 'compact' ? 'border-t-4' : 'border-t-[6px]'))
const bodyCellContentClass = computed(() =>
  props.density === 'compact'
    ? 'min-h-[calc(var(--cp-table-row-height-sm)-4px)]'
    : 'min-h-[calc(var(--cp-table-row-height)-6px)]',
)

function getRowKey(row: Row, index: number) {
  if (typeof props.rowKey === 'function')
    return props.rowKey(row, index)
  const value = cellValue(row, props.rowKey)
  return typeof value === 'string' || typeof value === 'number' ? value : index
}

function isRowSelected(row: Row, index: number) {
  return props.selectedRowKeys.includes(getRowKey(row, index))
}

function isRowExpanded(row: Row, index: number) {
  return props.expandedRowKeys.includes(getRowKey(row, index))
}

const expansionMotion = useTableExpansionMotion(
  () => scrollbarRef.value?.wrapRef,
  tableRef,
  () => props.scrollToExpanded,
)

function rowBackgroundClass(row: Row, index: number) {
  if (isRowSelected(row, index))
    return 'bg-(--cp-table-row-selected-bg)'
  if (index % 2 === 1)
    return 'bg-(--cp-table-row-stripe-bg)'
  return 'bg-(--cp-table-row-bg)'
}

function rowClass(row: Row, index: number) {
  return [
    bodyRowClass.value,
    isRowSelected(row, index)
      ? 'hover:[&>td]:bg-(--cp-table-row-selected-hover-bg)'
      : 'hover:[&>td]:bg-(--cp-table-row-hover-bg)',
  ]
}

function stickyClass(column: ResolvedTableColumn<Row>, header = false) {
  if (!column.sticky)
    return undefined
  const showShadow = column.sticky === 'left' ? horizontalScrolled.value : horizontalCanScrollRight.value
  return [
    'sticky',
    header ? 'z-40' : 'z-20',
    showShadow
      ? column.sticky === 'left'
        ? 'shadow-[8px_0_14px_-14px_var(--cp-color-shadow)]'
        : 'shadow-[-8px_0_14px_-14px_var(--cp-color-shadow)]'
      : undefined,
  ]
}

function bodyCellTitle(column: ResolvedTableColumn<Row>, row: Row) {
  return slots[column.key] ? undefined : cellTitle(column, row)
}

function columnSortDirection(column: ResolvedTableColumn<Row>) {
  return props.sort?.key === columnSortKey(column) ? props.sort.direction : undefined
}

function toggleColumnSort(column: ResolvedTableColumn<Row>) {
  const key = columnSortKey(column)
  const direction = columnSortDirection(column)
  if (!direction)
    emit('sortChange', { key, direction: 'asc' })
  else if (direction === 'asc')
    emit('sortChange', { key, direction: 'desc' })
  else emit('sortChange', undefined)
}

function columnAriaSort(column: ResolvedTableColumn<Row>) {
  const direction = columnSortDirection(column)
  if (direction === 'asc')
    return 'ascending'
  if (direction === 'desc')
    return 'descending'
  return column.sortable ? 'none' : undefined
}

function sortButtonLabel(column: ResolvedTableColumn<Row>) {
  const direction = columnSortDirection(column)
  if (!direction)
    return `${column.label || column.key}：升序排列`
  if (direction === 'asc')
    return `${column.label || column.key}：降序排列`
  return `${column.label || column.key}：取消排序`
}
</script>

<template>
  <div class="@container/table isolate flex h-full min-h-0 w-full max-w-full flex-col overflow-hidden">
    <div v-if="reorderable" class="flex shrink-0 items-center gap-2 px-4 py-2 text-cp-xs text-cp-text-secondary" aria-live="polite">
      <template v-if="sort">
        当前按列排序
        <button type="button" class="text-cp-primary-text" @click="emit('sortChange', undefined)">
          恢复拖动顺序
        </button>
      </template>
      <template v-else>
        拖动左侧手柄调整当前页顺序，自动保存并全站共享；也可聚焦手柄后按 Alt + ↑ / ↓
      </template>
    </div>
    <div v-if="horizontalControls && (horizontalScrolled || horizontalCanScrollRight)" class="flex shrink-0 items-center justify-end gap-2 px-4 pb-2 text-cp-xs text-cp-text-secondary">
      <span>更多列</span>
      <BaseButton size="sm" variant="secondary" aria-label="向左滚动表格" :disabled="!horizontalScrolled" @click="scrollHorizontally(-1)">
        <ChevronLeft class="size-4" />
      </BaseButton>
      <BaseButton size="sm" variant="secondary" aria-label="向右滚动表格" :disabled="!horizontalCanScrollRight" @click="scrollHorizontally(1)">
        <ChevronRight class="size-4" />
      </BaseButton>
    </div>
    <div v-loading="loading && (hasRows || !showHeaderWhenEmpty)" class="relative flex min-h-0 max-w-full flex-1 overflow-hidden">
      <BaseScrollbar
        v-if="hasRows || showHeaderWhenEmpty"
        ref="scrollbar"
        class="min-h-0 flex-1"
        :class="
          density === 'compact'
            ? '[--cp-scrollbar-track-inset-block-start:2.25rem]'
            : '[--cp-scrollbar-track-inset-block-start:2.75rem]'
        "
        :always-visible="scrollbarAlwaysVisible"
        horizontal
        @scroll="handleTableScroll"
      >
        <table ref="table" class="table-fixed border-separate border-spacing-0 text-left" :style="resolvedTableStyle">
          <colgroup>
            <col v-for="column in computedColumns" :key="column.key" :style="columnStyle(column, computedColumns)">
          </colgroup>
          <thead>
            <tr :class="headerRowClass">
              <th
                v-for="(column, columnIndex) in computedColumns"
                :key="column.key"
                class="sticky top-0 z-30 whitespace-nowrap bg-(--cp-table-header-bg) shadow-[0_10px_16px_-18px_var(--cp-color-shadow)]"
                :class="[
                  column.paddingClass ?? cellPaddingClass,
                  alignClass(column),
                  columnIndex === 0 ? 'rounded-l-cp' : undefined,
                  columnIndex === computedColumns.length - 1 ? 'rounded-r-cp' : undefined,
                  columnIndex === computedColumns.length - 1 ? 'pr-6' : undefined,
                  stickyClass(column, true),
                ]"
                :style="stickyStyle(column)"
                scope="col"
                :aria-sort="columnAriaSort(column)"
              >
                <div :class="cellContentClass(column)" :data-column-motion="column.sticky ? undefined : column.key">
                  <button
                    v-if="column.sortable"
                    type="button"
                    class="inline-flex max-w-full touch-manipulation items-center gap-1 border-0 bg-transparent p-0 text-inherit outline-none transition-colors hover:text-cp-text focus-visible:text-cp-primary-text motion-reduce:transition-none"
                    :aria-label="sortButtonLabel(column)"
                    :title="sortButtonLabel(column)"
                    @click="toggleColumnSort(column)"
                  >
                    <span class="truncate">
                      <slot :name="`header-${column.key}`" :column="column">{{ column.label }}</slot>
                    </span>
                    <span class="inline-flex shrink-0 -translate-y-px flex-col gap-px">
                      <Triangle
                        class="size-1.25 fill-current"
                        :class="
                          columnSortDirection(column) === 'asc' ? 'text-cp-primary-text' : 'text-cp-text-tertiary'
                        "
                        :stroke-width="0"
                      />
                      <Triangle
                        class="size-1.25 rotate-180 fill-current"
                        :class="
                          columnSortDirection(column) === 'desc' ? 'text-cp-primary-text' : 'text-cp-text-tertiary'
                        "
                        :stroke-width="0"
                      />
                    </span>
                  </button>
                  <slot v-else :name="`header-${column.key}`" :column="column">
                    {{ column.label }}
                  </slot>
                </div>
              </th>
            </tr>
          </thead>
          <tbody v-for="(row, index) in displayRows" :key="getRowKey(row, index)" data-row-group :data-row-key="getRowKey(row, index)">
            <tr :class="rowClass(row, index)" :aria-selected="isRowSelected(row, index) || undefined">
              <td
                v-for="(column, columnIndex) in computedColumns"
                :key="column.key"
                class="min-w-0 transition-[background-color] duration-150 ease-out motion-reduce:transition-none"
                :class="[
                  column.paddingClass ?? cellPaddingClass,
                  bodyTextClass,
                  bodyCellFrameClass,
                  column.contentClass,
                  alignClass(column),
                  index === 0 ? firstRowTopGapClass : undefined,
                  columnIndex === 0 ? 'rounded-l-cp' : undefined,
                  columnIndex === computedColumns.length - 1 ? 'rounded-r-cp pr-6' : undefined,
                  stickyClass(column),
                  rowBackgroundClass(row, index),
                ]"
                :style="stickyStyle(column)"
              >
                <div class="grid content-center" :class="bodyCellContentClass" :data-column-motion="column.sticky ? undefined : column.key">
                  <div :class="cellContentClass(column)" :title="bodyCellTitle(column, row)">
                    <button v-if="column.key === '__reorder'" type="button" data-row-handle :disabled="!canReorder" class="touch-none rounded p-1 text-cp-text-tertiary enabled:cursor-grab enabled:hover:text-cp-primary-text disabled:opacity-30" :aria-label="`调整第 ${index + 1} 行顺序`" title="拖动排序 · Alt + ↑ / ↓" @keydown="moveWithKeyboard($event, index)">
                      <GripVertical class="size-4" />
                    </button>
                    <slot
                      v-else
                      :name="column.key"
                      :row="row"
                      :value="cellValue(row, column.key)"
                      :display-value="cellDisplayValue(column, row)"
                      :index="index"
                    >
                      {{ cellDisplayValue(column, row) }}
                    </slot>
                  </div>
                </div>
              </td>
            </tr>
            <Transition
              :css="false"
              @enter="expansionMotion.enter"
              @leave="expansionMotion.leave"
              @enter-cancelled="expansionMotion.cancel"
              @leave-cancelled="expansionMotion.cancel"
            >
              <tr v-if="isRowExpanded(row, index)">
                <td :colspan="computedColumns.length" class="rounded-cp bg-cp-fill-quaternary p-0">
                  <div data-expansion-panel class="overflow-hidden">
                    <div><slot name="expanded" :row="row" :index="index" /></div>
                  </div>
                </td>
              </tr>
            </Transition>
          </tbody>
        </table>
      </BaseScrollbar>
      <div
        v-if="!hasRows"
        v-loading="loading && showHeaderWhenEmpty"
        class="grid min-h-0 flex-1 place-items-center overflow-hidden px-4"
        :class="showHeaderWhenEmpty ? ['absolute inset-x-0 bottom-0', density === 'compact' ? 'top-8' : 'top-10'] : undefined"
      >
        <slot v-if="!loading" name="empty">
          <BaseEmpty :title="emptyText" surface="none" class="w-full max-w-80" />
        </slot>
      </div>
    </div>
  </div>
</template>
