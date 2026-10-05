/**
 * 流量分区：网关请求流水（服务端分页），列定义也住这里。
 */

import { useCallback, useEffect, useState } from "react";
import type { DataTableColumn } from "../../components/data/DataTable";
import { DataTable } from "../../components/data/DataTable";
import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { Pagination } from "../../components/data/Pagination";
import type { Strings } from "../../i18n/strings";
import {
  fetchGatewayRequests,
  type GatewayRequestRow,
  type GatewayRequestsPage,
} from "../../lib/api";
import {
  formatCostMicros,
  formatCount,
  formatTimestamp,
  formatTokens,
} from "../../lib/format";
import { useNumberUnit, type NumberUnit } from "../../lib/numberUnit";
import { PaneCard } from "./PaneCard";

const TRAFFIC_PAGE_SIZE = 20;

interface TrafficColumnsArgs {
  readonly strings: Strings;
  readonly numberUnit: NumberUnit;
}

function trafficColumns({
  strings,
  numberUnit,
}: TrafficColumnsArgs): DataTableColumn<GatewayRequestRow>[] {
  return [
    {
      key: "ts",
      header: strings.detailsColTime,
      sortValue: (row) => row.ts,
      render: (row) => (
        <span className="font-mono text-xs">{formatTimestamp(row.ts)}</span>
      ),
    },
    {
      key: "model",
      header: strings.detailsColModel,
      render: (row) => row.model,
    },
    {
      key: "upstream",
      header: strings.gatewayColUpstream,
      render: (row) => row.upstream ?? "—",
    },
    {
      key: "status",
      header: strings.gatewayColStatus,
      render: (row) =>
        row.statusCode === null ? (
          <Badge tone="unknown">{strings.detailsCostUnknown}</Badge>
        ) : (
          <Badge tone={row.statusCode < 400 ? "ok" : "danger"}>
            {row.statusCode < 400
              ? strings.gatewayStatusOk
              : strings.gatewayStatusError}
          </Badge>
        ),
    },
    {
      key: "latency",
      header: strings.gatewayColLatency,
      numeric: true,
      render: (row) =>
        row.latencyMs === null ? "—" : `${formatCount(row.latencyMs)} ms`,
    },
    {
      key: "tokens",
      header: strings.detailsColTokens,
      numeric: true,
      sortValue: (row) => row.inputTokens + row.outputTokens,
      render: (row) =>
        formatTokens(row.inputTokens + row.outputTokens, numberUnit),
    },
    {
      key: "cost",
      header: strings.gatewayColCost,
      numeric: true,
      sortValue: (row) => row.costMicros ?? -1,
      render: (row) =>
        row.costMicros === null ? (
          <Badge tone="unknown">{strings.detailsCostUnknown}</Badge>
        ) : (
          <span className="tabular-nums">
            {formatCostMicros(row.costMicros)}
          </span>
        ),
    },
  ];
}

interface TrafficPaneProps {
  readonly strings: Strings;
  readonly overviewError: string | null;
}

export function TrafficPane({ strings, overviewError }: TrafficPaneProps) {
  const numberUnit = useNumberUnit();
  const [page, setPage] = useState(1);
  const [data, setData] = useState<GatewayRequestsPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(() => {
    setLoading(true);
    fetchGatewayRequests(page, TRAFFIC_PAGE_SIZE)
      .then((next) => {
        setData(next);
        setError(null);
      })
      .catch((err: unknown) => setError(String(err)))
      .finally(() => setLoading(false));
  }, [page]);

  useEffect(() => {
    load();
  }, [load]);

  const pageCount =
    data === null ? 1 : Math.max(1, Math.ceil(data.total / TRAFFIC_PAGE_SIZE));

  return (
    <PaneCard>
      <div className="flex items-center justify-between gap-3">
        <span className="text-sm">
          {formatCount(data?.total ?? 0)} {strings.gatewayTabTraffic}
        </span>
        <Button variant="ghost" onClick={load} disabled={loading}>
          {strings.gatewayRefresh}
        </Button>
      </div>

      {overviewError === null ? null : (
        <p className="mt-2 text-xs text-danger">{overviewError}</p>
      )}
      {error === null ? null : (
        <p className="mt-2 text-xs text-danger">{error}</p>
      )}

      <div className="mt-4 flex min-h-0 flex-1 flex-col">
        <DataTable
          columns={trafficColumns({ strings, numberUnit })}
          rows={data?.rows ?? []}
          rowKey={(row) => String(row.id)}
          empty={
            <EmptyState>
              <p>{strings.gatewayTrafficEmptyTitle}</p>
              <p className="mt-1 text-xs">
                {strings.gatewayTrafficEmptyDetail}
              </p>
            </EmptyState>
          }
          footer={
            data === null || data.total === 0 ? null : (
              <Pagination
                page={page}
                pageCount={pageCount}
                total={data.total}
                pageSize={TRAFFIC_PAGE_SIZE}
                pageSizeOptions={[TRAFFIC_PAGE_SIZE]}
                labels={{
                  nav: strings.paginationNav,
                  first: strings.paginationFirst,
                  prev: strings.paginationPrev,
                  next: strings.paginationNext,
                  last: strings.paginationLast,
                  total: strings.paginationTotal,
                  pageSize: strings.paginationPageSize,
                  pageSizeUnit: strings.paginationPageSizeUnit,
                  jump: strings.paginationJump,
                }}
                onPageChange={setPage}
                onPageSizeChange={() => {}}
              />
            )
          }
        />
      </div>
    </PaneCard>
  );
}
