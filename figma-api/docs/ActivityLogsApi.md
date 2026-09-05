# \ActivityLogsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_activity_logs**](ActivityLogsApi.md#get_activity_logs) | **GET** /v1/activity_logs | Get activity logs



## get_activity_logs

> models::InlineObject29 get_activity_logs(events, start_time, end_time, limit, order)
Get activity logs

Returns a list of activity log events

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**events** | Option<**String**> | Event type(s) to include in the response. Can have multiple values separated by comma. All events are returned by default. |  |
**start_time** | Option<**f64**> | Unix timestamp of the least recent event to include. This param defaults to one year ago if unspecified. |  |
**end_time** | Option<**f64**> | Unix timestamp of the most recent event to include. This param defaults to the current timestamp if unspecified. |  |
**limit** | Option<**f64**> | Maximum number of events to return. This param defaults to 1000 if unspecified. |  |
**order** | Option<**String**> | Event order by timestamp. This param can be either \"asc\" (default) or \"desc\". |  |[default to asc]

### Return type

[**models::InlineObject29**](inline_object_29.md)

### Authorization

[OrgOAuth2](../README.md#OrgOAuth2), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

