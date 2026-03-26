#!/usr/bin/with-contenv bashio

export RUST_BACKTRACE=full
export RUST_LOG_STYLE=always
export XDG_CACHE_HOME=/data
# export GOVEE_HTTP_INGRESS_ONLY=1

# Propagate timezone from Home Assistant
export TZ="$(bashio::supervisor.timezone)"

# Generic config-to-env helper
export_config() {
  local key="$1" var="$2"
  if bashio::config.has_value "$key" ; then
    export "$var"="$(bashio::config "$key")"
  fi
}

wait_for_mqtt() {
  local max_attempts=30
  local attempt=1

  bashio::log.info "mqtt_host was not explicitly configured, waiting for the Mosquitto broker app to become available"

  while [ $attempt -le $max_attempts ]; do
    if bashio::services.available mqtt ; then
      if timeout 2 bash -c "cat < /dev/null > /dev/tcp/$(bashio::services mqtt host)/$(bashio::services mqtt port)" 2>/dev/null; then
        bashio::log.info "MQTT broker is ready!"
        return 0
      fi
    fi

    bashio::log.info "MQTT broker not ready yet (attempt ${attempt}/${max_attempts}), waiting 2 seconds..."
    sleep 2
    attempt=$((attempt + 1))
  done

  bashio::log.error "MQTT broker did not become available after ${max_attempts} attempts"
  return 1
}

# MQTT configuration
if bashio::config.has_value mqtt_host ; then
  export GOVEE_MQTT_HOST="$(bashio::config mqtt_host)"
else
  if ! wait_for_mqtt ; then
    bashio::exit.nok "Mosquitto MQTT broker is not available"
  fi
  export GOVEE_MQTT_HOST="$(bashio::services mqtt 'host')"
  export GOVEE_MQTT_PORT="$(bashio::services mqtt 'port')"
  export GOVEE_MQTT_USER="$(bashio::services mqtt 'username')"
  export GOVEE_MQTT_PASSWORD="$(bashio::services mqtt 'password')"
fi

export_config mqtt_port         GOVEE_MQTT_PORT
export_config mqtt_username     GOVEE_MQTT_USER
export_config mqtt_password     GOVEE_MQTT_PASSWORD
export_config debug_level       RUST_LOG
export_config govee_email       GOVEE_EMAIL
export_config govee_password    GOVEE_PASSWORD
export_config govee_api_key     GOVEE_API_KEY
export_config no_multicast      GOVEE_LAN_NO_MULTICAST
export_config broadcast_all     GOVEE_LAN_BROADCAST_ALL
export_config global_broadcast  GOVEE_LAN_BROADCAST_GLOBAL
export_config scan              GOVEE_LAN_SCAN
export_config temperature_scale GOVEE_TEMPERATURE_SCALE
export_config disable_effects   GOVEE_DISABLE_EFFECTS
export_config allowed_effects   GOVEE_ALLOWED_EFFECTS
export_config poll_interval     GOVEE_POLL_INTERVAL

env | grep GOVEE_ | sed -r 's/_(EMAIL|KEY|PASSWORD)=.*/_\1=REDACTED/'
set -x

cd /app
exec /app/govee serve
