{{- define "beenet-registry.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "beenet-registry.fullname" -}}
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

{{- define "beenet-registry.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "beenet-registry.labels" -}}
helm.sh/chart: {{ include "beenet-registry.chart" . }}
{{ include "beenet-registry.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "beenet-registry.selectorLabels" -}}
app.kubernetes.io/name: {{ include "beenet-registry.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Redis URL used by the registry container.
  - redis.enabled=true  → built-in single-pod Redis Service
  - redis.enabled=false → externalRedis.url (must be set)
*/}}
{{- define "beenet-registry.redisUrl" -}}
{{- if .Values.redis.enabled }}
{{- printf "redis://%s-redis:%d" (include "beenet-registry.fullname" .) (.Values.redis.port | int) }}
{{- else }}
{{- required "externalRedis.url is required when redis.enabled=false" .Values.externalRedis.url }}
{{- end }}
{{- end }}
