use serde::Serialize;

use crate::{EventSink, error::AppError};

/// Reports the progress of a multi-step operation as `operation_<id>` events.
pub struct Operation<'a> {
    id: String,
    sink: &'a dyn EventSink,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationUpdate<'a> {
    update_type: &'a str,
    step_id: &'a str,
    extra_details: Option<AppError>,
}

impl<'a> Operation<'a> {
    pub fn new(id: String, sink: &'a dyn EventSink) -> Operation<'a> {
        Operation { id, sink }
    }

    fn send(&self, update: OperationUpdate) -> Result<(), AppError> {
        let payload =
            serde_json::to_value(update).map_err(|e| AppError::OperationUpdate(e.to_string()))?;
        self.sink.emit(&format!("operation_{}", self.id), payload);
        Ok(())
    }

    pub fn move_on(&self, old_id: &str, new_id: &str) -> Result<(), AppError> {
        self.complete(old_id)?;
        self.start(new_id)
    }

    pub fn start(&self, id: &str) -> Result<(), AppError> {
        self.send(OperationUpdate {
            update_type: "started",
            step_id: id,
            extra_details: None,
        })
    }

    pub fn complete(&self, id: &str) -> Result<(), AppError> {
        self.send(OperationUpdate {
            update_type: "finished",
            step_id: id,
            extra_details: None,
        })
    }

    pub fn fail<T>(&self, id: &str, error: AppError) -> Result<T, AppError> {
        self.send(OperationUpdate {
            update_type: "failed",
            step_id: id,
            extra_details: Some(error.clone()),
        })?;
        Err(error)
    }

    pub fn fail_if_err<T>(&self, id: &str, res: Result<T, AppError>) -> Result<T, AppError> {
        match res {
            Ok(t) => Ok(t),
            Err(e) => self.fail::<T>(id, e),
        }
    }
}
