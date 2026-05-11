# \CommentsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**delete_comment**](CommentsApi.md#delete_comment) | **DELETE** /v1/files/{file_key}/comments/{comment_id} | Delete a comment
[**get_comments**](CommentsApi.md#get_comments) | **GET** /v1/files/{file_key}/comments | Get comments in a file
[**post_comment**](CommentsApi.md#post_comment) | **POST** /v1/files/{file_key}/comments | Add a comment to a file



## delete_comment

> models::InlineObject9 delete_comment(file_key, comment_id)
Delete a comment

Deletes a specific comment. Only the person who made the comment is allowed to delete it.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to delete comment from. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**comment_id** | **String** | Comment id of comment to delete | [required] |

### Return type

[**models::InlineObject9**](inline_object_9.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_comments

> models::InlineObject8 get_comments(file_key, as_md)
Get comments in a file

Gets a list of comments left on the file.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to get comments from. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**as_md** | Option<**bool**> | If enabled, will return comments as their markdown equivalents when applicable. |  |

### Return type

[**models::InlineObject8**](inline_object_8.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## post_comment

> models::Comment post_comment(file_key, post_comment_request)
Add a comment to a file

Posts a new comment on the file.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to add comments in. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**post_comment_request** | [**PostCommentRequest**](PostCommentRequest.md) | Comment to post. | [required] |

### Return type

[**models::Comment**](Comment.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

