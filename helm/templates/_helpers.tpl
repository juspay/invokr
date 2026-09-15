{{/* Expand the name of the chart. */}}
{{- define "invokr.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Fully qualified app name. */}}
{{- define "invokr.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* Chart name and version, for the helm.sh/chart label. */}}
{{- define "invokr.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Common labels. */}}
{{- define "invokr.labels" -}}
helm.sh/chart: {{ include "invokr.chart" . }}
{{ include "invokr.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Selector labels shared by every workload. */}}
{{- define "invokr.selectorLabels" -}}
app.kubernetes.io/name: {{ include "invokr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/* Per-component selector labels; pass a dict of `root` and `component`.
     The component label stops the api and worker selecting each other's pods. */}}
{{- define "invokr.componentSelectorLabels" -}}
{{- $root := .root -}}
{{ include "invokr.selectorLabels" $root }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{/* Per-component labels. */}}
{{- define "invokr.componentLabels" -}}
{{- $root := .root -}}
{{ include "invokr.labels" $root }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{/* Name of the ServiceAccount to use. */}}
{{- define "invokr.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "invokr.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* The Secret holding the three INVOKR_* secrets: either the operator's
     `existingSecret` or the one this chart renders. */}}
{{- define "invokr.secretName" -}}
{{- if .Values.existingSecret }}
{{- .Values.existingSecret }}
{{- else }}
{{- printf "%s-secrets" (include "invokr.fullname" .) }}
{{- end }}
{{- end }}

{{/* Resolve a container image; pass a dict of `root` and `repository`.
     Registry precedence is global.imageRegistry then image.registry, so an ECR
     mirror is one value. `kms.enabled` appends the -kms suffix. */}}
{{- define "invokr.image" -}}
{{- $root := .root -}}
{{- $repository := .repository -}}
{{- if $root.Values.kms.enabled -}}
{{- $repository = printf "%s-kms" $repository -}}
{{- end -}}
{{- $registry := $root.Values.global.imageRegistry | default $root.Values.image.registry -}}
{{/* toString: a numeric tag parses as int64, which %s mangles. */}}
{{- $tag := $root.Values.image.tag | default $root.Chart.AppVersion | toString -}}
{{- printf "%s/%s:%s" $registry $repository $tag -}}
{{- end -}}

{{/* Render a values map as INVOKR_<KEY_UPPERCASED> ConfigMap entries.
     Only nil is skipped, so `false` and `0` survive -- an `if $value` guard
     would silently drop them. */}}
{{- define "invokr.configsToData" -}}
{{- range $key, $value := . }}
{{- if not (kindIs "invalid" $value) }}
INVOKR_{{ $key | upper }}: {{ $value | quote }}
{{- end }}
{{- end }}
{{- end }}

{{/* Same, base64 encoded. Empty values are skipped so a partial `secrets`
     block cannot overwrite real values with blanks. */}}
{{- define "invokr.secretsToData" -}}
{{- range $key, $value := . }}
{{- if $value }}
INVOKR_{{ $key | upper }}: {{ $value | b64enc }}
{{- end }}
{{- end }}
{{- end }}

{{/* API path prefix, normalised. Probes, ingress paths and the
     ServiceMonitor path all derive from this so they cannot drift. */}}
{{- define "invokr.pathPrefix" -}}
{{- $prefix := .Values.configs.path_prefix | default "" -}}
{{- if $prefix -}}
{{- printf "/%s" (trimAll "/" $prefix) -}}
{{- end -}}
{{- end -}}

{{/* Node scheduling: chart values win, falling back to global.*. */}}
{{- define "invokr.nodeSelector" -}}
{{- $v := .Values.nodeSelector | default .Values.global.nodeSelector -}}
{{- with $v }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}

{{- define "invokr.tolerations" -}}
{{- $v := .Values.tolerations | default .Values.global.tolerations -}}
{{- with $v }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}

{{- define "invokr.affinity" -}}
{{- $v := .Values.affinity | default .Values.global.affinity -}}
{{- with $v }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}

{{/* Fail rendering with a specific message rather than deploying something
     that cannot work. Called once from configmap.yaml, which always renders. */}}
{{- define "invokr.validateValues" -}}
{{- if not .Values.existingSecret -}}
  {{- if not .Values.secrets.database_url -}}
    {{- fail "invokr: set secrets.database_url (or existingSecret). Pods crash-loop without a database connection string. It must be the WRITER endpoint -- pg_cron runs jobs only on the writer." -}}
  {{- end -}}
  {{- if not .Values.secrets.api_key -}}
    {{- fail "invokr: set secrets.api_key (or existingSecret). It is the bearer token for every REST API call." -}}
  {{- end -}}
  {{- if not .Values.secrets.encryption_key -}}
    {{- fail "invokr: set secrets.encryption_key (or existingSecret). 32 bytes of hex; it encrypts stored secrets at rest." -}}
  {{- end -}}
{{- end -}}
{{- if and .Values.migration.enabled (not .Values.database.host) -}}
  {{- fail "invokr: migration.enabled is true, so database.host must be set for the Job's pg_isready check." -}}
{{- end -}}
{{- if and .Values.api.podDisruptionBudget.enabled (not .Values.api.autoscaling.enabled) -}}
  {{- if ge (int .Values.api.podDisruptionBudget.minAvailable) (int .Values.api.replicaCount) -}}
    {{- fail "invokr: api.podDisruptionBudget.minAvailable must be less than api.replicaCount, or nodes running the API can never be drained." -}}
  {{- end -}}
{{- end -}}
{{- if and .Values.worker.podDisruptionBudget.enabled (not .Values.worker.autoscaling.enabled) -}}
  {{- if ge (int .Values.worker.podDisruptionBudget.minAvailable) (int .Values.worker.replicaCount) -}}
    {{- fail "invokr: worker.podDisruptionBudget.minAvailable must be less than worker.replicaCount, or nodes running the worker can never be drained." -}}
  {{- end -}}
{{- end -}}
{{- end -}}

{{/* Per-workload config checksums, so a worker-only change does not roll the API. */}}
{{- define "invokr.apiConfigChecksum" -}}
{{- printf "%s|%s|%s|%v" (toYaml .Values.configs) (toYaml .Values.apiConfigs) (toYaml .Values.dashboard) .Values.api.service.targetPort | sha256sum -}}
{{- end -}}

{{- define "invokr.workerConfigChecksum" -}}
{{- printf "%s|%s" (toYaml .Values.configs) (toYaml .Values.workerConfigs) | sha256sum -}}
{{- end -}}
