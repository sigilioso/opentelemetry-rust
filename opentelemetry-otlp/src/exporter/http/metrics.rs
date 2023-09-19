use std::sync::Arc;

use async_trait::async_trait;
use http::{header::CONTENT_TYPE, Method};
use opentelemetry::metrics::{MetricsError, Result};
use opentelemetry_sdk::metrics::data::ResourceMetrics;

use crate::{metric::MetricsClient, Error};

use super::OtlpHttpClient;

#[async_trait]
impl MetricsClient for OtlpHttpClient {
    async fn export(&self, metrics: &mut ResourceMetrics) -> Result<()> {
        let client = self
            .client
            .lock()
            .map_err(Into::into)
            .and_then(|g| match &*g {
                Some(client) => Ok(Arc::clone(client)),
                _ => Err(MetricsError::Other("exporter is already shut down".into())),
            })?;

        let (body, content_type) = build_body(metrics)?;
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri(&self.collector_endpoint)
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .map_err(|e| crate::Error::RequestFailed(Box::new(e)))?;

        for (k, v) in &self.headers {
            request.headers_mut().insert(k.clone(), v.clone());
        }

        let response = client
            .send(request)
            .await
            .map_err(|e| MetricsError::ExportErr(Box::new(Error::RequestFailed(e))))?;

        // TODO: use `opentelemetry_http::ResponseExt` instead (currently it returns TraceError)
        if !response.status().is_success() {
            let body_msg = std::str::from_utf8(response.body().iter().as_slice())
                .unwrap_or("response body cannot be decoded");
            return Err(MetricsError::ExportErr(Box::new(Error::RequestFailed(
                format!(
                    "request failed with status {} (Body: {})",
                    response.status(),
                    body_msg
                )
                .into(),
            ))))?;
        }

        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        let _ = self.client.lock()?.take();

        Ok(())
    }
}

#[cfg(feature = "http-proto")]
fn build_body(metrics: &mut ResourceMetrics) -> Result<(Vec<u8>, &'static str)> {
    use prost::Message;

    let req: opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest =
        (&*metrics).into();
    let mut buf = vec![];
    req.encode(&mut buf).map_err(crate::Error::from)?;

    Ok((buf, "application/x-protobuf"))
}

#[cfg(not(feature = "http-proto"))]
fn build_body(metrics: &mut ResourceMetrics) -> Result<(Vec<u8>, &'static str)> {
    Err(MetricsError::Other(
        "No http protocol configured. Enable one via `http-proto`".into(),
    ))
}


#[cfg(test)]
mod tests {
    // TODO: check if this tests could be implemented in a more generic way
    use async_trait::async_trait;
    use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
    use opentelemetry_sdk::{
        metrics::{data::ResourceMetrics, exporter::PushMetricsExporter},
        Resource,
    };

    #[derive(Debug, Default)]
    struct MockClient {
        response_bytes: Bytes,
        status_code: http::StatusCode,
    }

    #[async_trait]
    impl HttpClient for MockClient {
        async fn send(&self, _: Request<Vec<u8>>) -> Result<Response<Bytes>, HttpError> {
            let response = Response::<Bytes>::new(self.response_bytes.clone());
            let (mut parts, body) = response.into_parts();
            parts.status = self.status_code;
            Ok(Response::from_parts(parts, body))
        }
    }

    #[tokio::test]
    async fn test_bad_status_code_error_message() {
        let client = MockClient {
            response_bytes: Bytes::from("Details"),
            status_code: http::StatusCode::BAD_REQUEST,
        };
        let exporter = opentelemetry_otlp::new_exporter()
            .http()
            .with_http_client(client)
            .build_metrics_exporter(
                Box::new(opentelemetry_sdk::metrics::reader::DefaultAggregationSelector::new()),
                Box::new(opentelemetry_sdk::metrics::reader::DefaultTemporalitySelector::new()),
            )
            .unwrap();
        let mut metrics = ResourceMetrics {
            resource: Resource::default(),
            scope_metrics: vec![],
        };
        let result = exporter.export(&mut metrics).await.unwrap_err();
        let debug_err = format!("{:?}", result);
        assert!(debug_err.contains("400"));
        assert!(debug_err.contains("Details"));
    }
}
